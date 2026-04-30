pub fn format_elapsed_ms(elapsed: f64) -> String {
    if elapsed < 1000.0 {
        format!("{}ms", elapsed as u64)
    } else if elapsed < 60_000.0 {
        format!("{:.2}s", elapsed / 1000.0)
    } else {
        let total_seconds = (elapsed / 1000.0) as u64;
        format!("{}m{}s", total_seconds / 60, total_seconds % 60)
    }
}
