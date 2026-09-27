use super::MAX_STREAM_CAPTURE_BYTES;

pub(super) fn cap_capture(mut output: String) -> String {
    if output.len() > MAX_STREAM_CAPTURE_BYTES {
        let mut length = MAX_STREAM_CAPTURE_BYTES;
        while !output.is_char_boundary(length) {
            length -= 1;
        }
        output.truncate(length);
    }
    output
}

#[cfg(test)]
pub(super) fn append_capped_stdout(output: &mut String, line: &str) {
    let remaining = MAX_STREAM_CAPTURE_BYTES.saturating_sub(output.len());
    if remaining == 0 {
        return;
    }
    let mut take = line.len().min(remaining);
    while !line.is_char_boundary(take) {
        take = take.saturating_sub(1);
    }
    output.push_str(&line[..take]);
    if output.len() < MAX_STREAM_CAPTURE_BYTES {
        output.push('\n');
    }
}

#[cfg(test)]
pub(super) fn append_capped_bytes(output: &mut Vec<u8>, chunk: &[u8]) {
    let remaining = MAX_STREAM_CAPTURE_BYTES.saturating_sub(output.len());
    output.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
}
