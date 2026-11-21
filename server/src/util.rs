//! Small shared helpers.

use data_encoding::HEXLOWER;

/// `N` bytes from the operating system's CSPRNG.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("operating system random number generator unavailable");
    b
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    HEXLOWER.encode(bytes)
}

/// Current wall-clock time, Unix milliseconds.
pub fn system_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
