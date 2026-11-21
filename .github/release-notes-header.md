> **Beta: expect bugs; back up your vault.**

**Windows:** the installer is not code-signed yet, so SmartScreen may warn
"Windows protected your PC". Click **More info**, then **Run anyway**.

**macOS:** the app is signed with a Developer ID and notarized by Apple.

**Verify your download:** every file is listed in `SHA256SUMS` and has a
build-provenance attestation (`gh attestation verify <file> --repo clym252/termlyx`).
