use std::fs;

fn read_or_unavailable(path: &str) -> String {
    match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(_) => format!("{path}: not available"),
    }
}

fn main() {
    println!("=== m3OS System Information ===");
    println!();

    println!("--- Memory Info ---");
    println!("{}", read_or_unavailable("/proc/meminfo"));

    println!("--- Uptime ---");
    println!("{}", read_or_unavailable("/proc/uptime"));
}
