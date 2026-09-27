# Mobile Linux Runtime

A standalone Android and iOS Linux runtime SDK, extracted from LingXi and Harness.
The SDK owns shared execution contracts, verified rootfs lifecycle, Android PRoot,
and the iOS arm64 iSH backend. It has no dependency on an Agent or LLM runtime.

The initial extraction is in progress on `codex/extract-mobile-linux-runtime`.
Source and native artifact provenance are recorded under `docs/migration`.
Platform support, source and binary integration, and validation results are
recorded with the release rather than inferred from a successful compilation.

Third-party sources retain their individual licenses and notices. The workspace
license applies only to files originally distributed under those terms.
