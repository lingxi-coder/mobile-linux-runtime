use std::collections::HashMap;
#[cfg(unix)]
use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use tokio::sync::mpsc;

use crate::spawn_pty_process;
use crate::ProcessSignal;
use crate::SpawnedProcess;
use crate::TerminalSize;

fn environment() -> HashMap<String, String> {
    std::env::vars().collect()
}

#[cfg(unix)]
async fn spawn_shell(script: &str, size: TerminalSize) -> anyhow::Result<SpawnedProcess> {
    spawn_pty_process(
        "/bin/sh",
        &["-c".to_owned(), script.to_owned()],
        Path::new("/"),
        &environment(),
        &None,
        size,
        &[],
    )
    .await
}

// ConPTY output is a terminal presentation stream, so even a first marker
// can be preceded by CSI cursor/erase sequences or an OSC window title.
fn terminal_text(bytes: &[u8]) -> String {
    let mut visible = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b {
            visible.push(bytes[index]);
            index += 1;
            continue;
        }
        index += 1;
        match bytes.get(index) {
            Some(b'[') => {
                index += 1;
                while index < bytes.len() {
                    let final_byte = (0x40..=0x7e).contains(&bytes[index]);
                    index += 1;
                    if final_byte {
                        break;
                    }
                }
            }
            Some(b']') => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == 7 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            Some(_) => index += 1,
            None => {}
        }
    }
    String::from_utf8_lossy(&visible).replace('\r', "")
}

fn numeric_marker(output: &[u8], marker: &str) -> anyhow::Result<u32> {
    let text = terminal_text(output);
    let digits = text
        .split_once(marker)
        .map(|(_, value)| {
            value
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .ok_or_else(|| anyhow::anyhow!("missing marker {marker:?} in {text:?}"))?;
    digits
        .parse()
        .with_context(|| format!("invalid numeric marker {marker:?} in {text:?}"))
}

fn output_diagnostic(collected: &[u8]) -> String {
    let tail = &collected[collected.len().saturating_sub(4096)..];
    format!(
        "received {} bytes; raw tail={:?}; visible tail={:?}",
        collected.len(),
        String::from_utf8_lossy(tail),
        terminal_text(tail)
    )
}

async fn read_until_with_timeout(
    output: &mut mpsc::Receiver<Vec<u8>>,
    marker: &[u8],
    timeout: Duration,
) -> anyhow::Result<Vec<u8>> {
    let mut collected = Vec::new();
    let marker_text = String::from_utf8_lossy(marker);
    let found = tokio::time::timeout(timeout, async {
        while let Some(chunk) = output.recv().await {
            collected.extend_from_slice(&chunk);
            if terminal_text(&collected).contains(marker_text.as_ref()) {
                return true;
            }
        }
        false
    })
    .await;
    match found {
        Ok(true) => Ok(collected),
        Ok(false) => anyhow::bail!(
            "PTY output closed before marker {marker_text:?}; {}",
            output_diagnostic(&collected)
        ),
        Err(_) => anyhow::bail!(
            "timed out after {timeout:?} waiting for PTY marker {marker_text:?}; {}",
            output_diagnostic(&collected)
        ),
    }
}

async fn read_until(
    output: &mut mpsc::Receiver<Vec<u8>>,
    marker: &[u8],
) -> anyhow::Result<Vec<u8>> {
    read_until_with_timeout(output, marker, Duration::from_secs(15)).await
}

#[test]
fn numeric_marker_accepts_conpty_csi_and_osc_prefixes() {
    let observed =
        b"\x1b[2J\x1b[m\x1b[H\x1b]0;Administrator: powershell.exe\x07\x1b[?25h__CHILD__4452\r\n";
    assert_eq!(numeric_marker(observed, "__CHILD__").unwrap(), 4452);
    let title_has_fake_marker = b"\x1b]0;__CHILD__99\x1b\\__CHILD__4452__PID_END__\r\n";
    assert_eq!(
        numeric_marker(title_has_fake_marker, "__CHILD__").unwrap(),
        4452
    );
    assert!(numeric_marker(b"__CHILD__invalid\n", "__CHILD__").is_err());
}

#[tokio::test]
async fn marker_timeout_reports_captured_bytes_and_expected_marker() {
    let (sender, mut receiver) = mpsc::channel(2);
    sender.send(b"\x1b[2J__GOT__??".to_vec()).await.unwrap();
    let error = read_until_with_timeout(
        &mut receiver,
        "__GOT__你好".as_bytes(),
        Duration::from_millis(10),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("__GOT__你好"), "{error}");
    assert!(error.contains("__GOT__??"), "{error}");
    assert!(error.contains("received 13 bytes"), "{error}");
}

#[cfg(windows)]
async fn wait_for_exit(spawned: &mut SpawnedProcess, phase: &str) -> anyhow::Result<i32> {
    match tokio::time::timeout(Duration::from_secs(15), &mut spawned.exit_rx).await {
        Ok(result) => result.with_context(|| format!("{phase}: PTY exit notification disappeared")),
        Err(_) => {
            let mut pending = Vec::new();
            while let Ok(chunk) = spawned.stdout_rx.try_recv() {
                pending.extend(chunk);
            }
            anyhow::bail!(
                "{phase}: child {:?} did not exit, observed status {:?}; {}",
                spawned.session.process_id(),
                spawned.session.exit_code(),
                output_diagnostic(&pending)
            )
        }
    }
}

async fn read_startup_preserving_output(
    output: &mut mpsc::Receiver<Vec<u8>>,
    marker: &[u8],
    timeout: Duration,
) -> anyhow::Result<()> {
    let initial = read_until_with_timeout(output, marker, timeout).await?;
    let (sender, receiver) = mpsc::channel(64);
    sender.send(initial).await?;
    let mut source = std::mem::replace(output, receiver);
    tokio::spawn(async move {
        while let Some(chunk) = source.recv().await {
            if sender.send(chunk).await.is_err() {
                break;
            }
        }
    });
    Ok(())
}

#[tokio::test]
async fn startup_handshake_preserves_coalesced_and_subsequent_output() -> anyhow::Result<()> {
    let (sender, mut receiver) = mpsc::channel(2);
    sender.send(b"__STARTED____READY__".to_vec()).await?;
    sender.send(b"__TAIL__".to_vec()).await?;
    drop(sender);
    read_startup_preserving_output(&mut receiver, b"__STARTED__", Duration::from_secs(1)).await?;
    assert_eq!(receiver.recv().await.unwrap(), b"__STARTED____READY__");
    assert_eq!(receiver.recv().await.unwrap(), b"__TAIL__");
    assert!(receiver.recv().await.is_none());
    Ok(())
}

#[cfg(windows)]
async fn spawn_powershell(script: &str, size: TerminalSize) -> anyhow::Result<SpawnedProcess> {
    let cwd = std::env::current_dir()?;
    let mut spawned = spawn_pty_process(
        "powershell.exe",
        &[
            "-NoLogo".to_owned(),
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!("[Console]::Out.WriteLine('__SHELL_STARTED__'); {script}"),
        ],
        &cwd,
        &environment(),
        &None,
        size,
        &[],
    )
    .await?;
    // PowerShell cold startup can exceed the operation deadline when native
    // PTY regressions launch concurrently on CI. Establish readiness separately;
    // interaction, EOF, resize and termination keep their existing 15s bound.
    read_startup_preserving_output(
        &mut spawned.stdout_rx,
        b"__SHELL_STARTED__",
        Duration::from_secs(60),
    )
    .await?;
    Ok(spawned)
}

#[cfg(windows)]
fn windows_process_is_running(process_id: u32) -> bool {
    use std::os::windows::io::RawHandle;

    use winapi::shared::minwindef::DWORD;
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::minwinbase::STILL_ACTIVE;
    use winapi::um::processthreadsapi::GetExitCodeProcess;
    use winapi::um::processthreadsapi::OpenProcess;
    use winapi::um::winnt::PROCESS_QUERY_LIMITED_INFORMATION;

    let handle: RawHandle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) as RawHandle };
    if handle.is_null() {
        return false;
    }

    let mut exit_code: DWORD = 0;
    let queried = unsafe { GetExitCodeProcess(handle.cast(), &mut exit_code) };
    unsafe {
        CloseHandle(handle.cast());
    }
    queried != 0 && exit_code == STILL_ACTIVE
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_interaction_and_wait_are_lossless() -> anyhow::Result<()> {
    let mut spawned = spawn_shell(
        "stty -echo; printf '__READY__\\n'; IFS= read -r line; printf '__GOT__%s\\n' \"$line\"",
        TerminalSize::default(),
    )
    .await?;
    read_until(&mut spawned.stdout_rx, b"__READY__").await?;

    spawned
        .session
        .write("你好, PTY\n".as_bytes().to_vec())
        .await?;
    let output = read_until(&mut spawned.stdout_rx, "__GOT__你好, PTY".as_bytes()).await?;
    assert!(String::from_utf8_lossy(&output).contains("__GOT__你好, PTY"));

    let (wait_code, exit_code) = tokio::join!(spawned.session.wait(), spawned.exit_rx);
    assert_eq!(wait_code, 0);
    assert_eq!(exit_code?, 0);
    assert!(spawned.session.has_exited());
    assert_eq!(spawned.session.exit_code(), Some(0));
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_tail_remains_readable_after_exit_notification() -> anyhow::Result<()> {
    let mut spawned = spawn_shell("printf '__TAIL_AFTER_WAIT__'", TerminalSize::default()).await?;

    // The child waiter and PTY reader are intentionally independent. Even
    // when wait wins the race, the reader must remain alive long enough to
    // publish bytes written immediately before exit.
    assert_eq!(spawned.exit_rx.await?, 0);
    let output = read_until(&mut spawned.stdout_rx, b"__TAIL_AFTER_WAIT__").await?;
    assert!(String::from_utf8_lossy(&output).contains("__TAIL_AFTER_WAIT__"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resize_reaches_the_child_terminal() -> anyhow::Result<()> {
    let mut spawned = spawn_shell(
        "stty -echo; stty size; printf '__READY__\\n'; IFS= read -r line; stty size",
        TerminalSize { rows: 24, cols: 80 },
    )
    .await?;
    let initial = read_until(&mut spawned.stdout_rx, b"__READY__").await?;
    assert!(String::from_utf8_lossy(&initial).contains("24 80"));

    spawned.session.resize(TerminalSize {
        rows: 47,
        cols: 133,
    })?;
    spawned.session.write(b"continue\n".to_vec()).await?;
    let resized = read_until(&mut spawned.stdout_rx, b"47 133").await?;
    assert!(String::from_utf8_lossy(&resized).contains("47 133"));
    assert_eq!(spawned.exit_rx.await?, 0);
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_targets_the_foreground_process_group() -> anyhow::Result<()> {
    let mut spawned = spawn_shell(
        "trap 'printf __INTERRUPTED__; exit 42' INT; printf '__READY__\\n'; while :; do sleep 1; done",
        TerminalSize::default(),
    )
    .await?;
    read_until(&mut spawned.stdout_rx, b"__READY__").await?;
    spawned.session.signal(ProcessSignal::Interrupt)?;
    let output = read_until(&mut spawned.stdout_rx, b"__INTERRUPTED__").await?;
    assert!(String::from_utf8_lossy(&output).contains("__INTERRUPTED__"));
    assert_eq!(spawned.exit_rx.await?, 42);
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminate_kills_descendants_in_the_same_process_group() -> anyhow::Result<()> {
    let mut spawned = spawn_shell(
        "sleep 1000 & child=$!; printf '__CHILD__%s\\n' \"$child\"; wait",
        TerminalSize::default(),
    )
    .await?;
    let output = read_until(&mut spawned.stdout_rx, b"\n").await?;
    let text = String::from_utf8_lossy(&output).replace('\r', "");
    let child_pid: i32 = text
        .lines()
        .find_map(|line| line.strip_prefix("__CHILD__"))
        .ok_or_else(|| anyhow::anyhow!("missing child PID in {text:?}"))?
        .parse()?;

    spawned.session.request_terminate();
    let exit_code = tokio::time::timeout(Duration::from_secs(5), spawned.exit_rx).await??;
    assert_ne!(exit_code, 0);

    let child_gone = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let result = unsafe { libc::kill(child_pid, 0) };
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        child_gone.is_ok(),
        "descendant {child_pid} survived PTY termination"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_stdin_delivers_eof() -> anyhow::Result<()> {
    let mut spawned = spawn_shell(
        "stty -echo; printf '__READY__\\n'; cat >/dev/null; printf '__EOF__'",
        TerminalSize::default(),
    )
    .await?;
    read_until(&mut spawned.stdout_rx, b"__READY__").await?;
    spawned.session.close_stdin();
    let output = read_until(&mut spawned.stdout_rx, b"__EOF__").await?;
    assert!(String::from_utf8_lossy(&output).contains("__EOF__"));
    assert_eq!(spawned.exit_rx.await?, 0);
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_zero_terminal_dimensions() -> anyhow::Result<()> {
    let error = spawn_shell("true", TerminalSize { rows: 0, cols: 80 })
        .await
        .expect_err("zero dimensions must fail");
    assert!(error.to_string().contains("greater than zero"));
    Ok(())
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conpty_raw_interaction_and_wait_are_lossless() -> anyhow::Result<()> {
    assert!(crate::conpty_supported(), "CI host must provide ConPTY");
    // Windows PowerShell uses .NET Framework: UTF-8 InputEncoding selects
    // ReadFile's legacy cooked-codepage path. Unicode selects ReadConsoleW;
    // ConPTY still receives UTF-8 bytes from the terminal side below.
    // https://github.com/microsoft/referencesource/blob/main/mscorlib/system/console.cs
    let mut spawned = spawn_powershell(
        "[Console]::InputEncoding = [Text.Encoding]::Unicode; \
         $OutputEncoding = [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); \
         Write-Output '__READY__'; \
         $line = [Console]::In.ReadLine(); \
         Write-Output ('__GOT__' + $line)",
        TerminalSize::default(),
    )
    .await?;
    read_until(&mut spawned.stdout_rx, b"__READY__").await?;

    let input = "你好, Windows ConPTY\n".as_bytes();
    // Split inside a UTF-8 character to exercise transport chunk boundaries.
    spawned.session.write(input[..2].to_vec()).await?;
    spawned.session.write(input[2..].to_vec()).await?;
    let marker = "__GOT__你好, Windows ConPTY";
    let output = read_until(&mut spawned.stdout_rx, marker.as_bytes()).await?;
    assert!(terminal_text(&output).contains(marker));
    assert_eq!(wait_for_exit(&mut spawned, "command completion").await?, 0);
    assert_eq!(spawned.session.wait().await, 0);
    Ok(())
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conpty_output_tail_remains_readable_after_exit() -> anyhow::Result<()> {
    let mut spawned = spawn_powershell(
        "[Console]::Out.Write('__TAIL_AFTER_WAIT__')",
        TerminalSize::default(),
    )
    .await?;

    assert_eq!(wait_for_exit(&mut spawned, "command completion").await?, 0);
    let output = read_until(&mut spawned.stdout_rx, b"__TAIL_AFTER_WAIT__").await?;
    assert!(terminal_text(&output).contains("__TAIL_AFTER_WAIT__"));
    Ok(())
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conpty_resize_reaches_the_child_terminal() -> anyhow::Result<()> {
    let mut spawned = spawn_powershell(
        "Write-Output '__READY__'; \
         [void][Console]::In.ReadLine(); \
         Start-Sleep -Milliseconds 100; \
         Write-Output ('__SIZE__{0} {1}' -f [Console]::WindowHeight, [Console]::WindowWidth)",
        TerminalSize { rows: 24, cols: 80 },
    )
    .await?;
    read_until(&mut spawned.stdout_rx, b"__READY__").await?;

    spawned.session.resize(TerminalSize {
        rows: 47,
        cols: 133,
    })?;
    spawned.session.write(b"continue\n".to_vec()).await?;
    let output = read_until(&mut spawned.stdout_rx, b"__SIZE__47 133").await?;
    assert!(terminal_text(&output).contains("__SIZE__47 133"));
    assert_eq!(wait_for_exit(&mut spawned, "command completion").await?, 0);
    Ok(())
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conpty_job_object_termination_kills_descendants() -> anyhow::Result<()> {
    let mut spawned = spawn_powershell(
        "$child = Start-Process powershell.exe \
             -ArgumentList '-NoLogo','-NoProfile','-Command','Start-Sleep -Seconds 1000' \
             -PassThru; \
         Write-Output ('__CHILD__' + $child.Id + '__PID_END__'); \
         Wait-Process -Id $child.Id",
        TerminalSize::default(),
    )
    .await?;
    let output = read_until(&mut spawned.stdout_rx, b"__PID_END__").await?;
    let child_pid = numeric_marker(&output, "__CHILD__")?;
    assert!(windows_process_is_running(child_pid));

    spawned.session.request_terminate();
    let exit_code = wait_for_exit(&mut spawned, "Job Object termination").await?;
    assert_ne!(exit_code, 0);

    tokio::time::timeout(Duration::from_secs(5), async {
        while windows_process_is_running(child_pid) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("descendant {child_pid} survived Job Object termination"))?;
    Ok(())
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conpty_close_stdin_delivers_eof() -> anyhow::Result<()> {
    let mut spawned = spawn_powershell(
        "Write-Output '__READY__'; \
         [void][Console]::In.ReadToEnd(); \
         Write-Output '__EOF__'",
        TerminalSize::default(),
    )
    .await?;
    read_until(&mut spawned.stdout_rx, b"__READY__").await?;

    spawned.session.close_stdin();
    let output = read_until(&mut spawned.stdout_rx, b"__EOF__").await?;
    assert!(terminal_text(&output).contains("__EOF__"));
    assert_eq!(wait_for_exit(&mut spawned, "command completion").await?, 0);
    Ok(())
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conpty_reports_unsupported_interactive_interrupt() -> anyhow::Result<()> {
    let mut spawned =
        spawn_powershell("Start-Sleep -Seconds 1000", TerminalSize::default()).await?;
    let error = spawned
        .session
        .signal(ProcessSignal::Interrupt)
        .expect_err("Windows ConPTY cannot synthesize a Unix SIGINT");
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    spawned.session.terminate();
    assert_ne!(wait_for_exit(&mut spawned, "forced termination").await?, 0);
    Ok(())
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conpty_rejects_zero_terminal_dimensions() -> anyhow::Result<()> {
    let error = spawn_powershell("exit 0", TerminalSize { rows: 24, cols: 0 })
        .await
        .expect_err("zero dimensions must fail");
    assert!(error.to_string().contains("greater than zero"));
    Ok(())
}
