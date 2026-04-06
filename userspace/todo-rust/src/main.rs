use std::env;
use std::fs;
use std::process;

const TODO_FILE: &str = "/data/todos.txt";

fn load_todos() -> Vec<String> {
    match fs::read_to_string(TODO_FILE) {
        Ok(contents) => contents.lines().map(String::from).filter(|l| !l.is_empty()).collect(),
        Err(_) => Vec::new(),
    }
}

fn save_todos(todos: &[String]) {
    let contents = todos.join("\n");
    let contents = if contents.is_empty() {
        contents
    } else {
        contents + "\n"
    };
    if let Err(e) = fs::write(TODO_FILE, contents) {
        eprintln!("{}: failed to write {TODO_FILE}: {e}", prog_name());
        process::exit(1);
    }
}

fn prog_name() -> String {
    env::args()
        .next()
        .and_then(|s| s.rsplit('/').next().map(String::from))
        .unwrap_or_else(|| "todo-rust".to_string())
}

fn print_usage() {
    let name = prog_name();
    println!("Usage: {name} <command> [args]");
    println!();
    println!("Commands:");
    println!("  add <text>   Add a new todo item");
    println!("  list         List all todo items");
    println!("  done <num>   Mark todo #num as complete (1-indexed)");
    println!("  help         Show this help message");
}

fn cmd_add(text: &str) {
    let mut todos = load_todos();
    todos.push(format!("[ ] {text}"));
    save_todos(&todos);
    println!("Added: {text}");
}

fn cmd_list() {
    let todos = load_todos();
    if todos.is_empty() {
        println!("No todos yet. Use '{} add <text>' to add one.", prog_name());
        return;
    }
    for (i, item) in todos.iter().enumerate() {
        println!("{:>3}. {item}", i + 1);
    }
}

fn cmd_done(num_str: &str) {
    let num: usize = match num_str.parse() {
        Ok(n) if n >= 1 => n,
        _ => {
            eprintln!("{}: invalid number: {num_str}", prog_name());
            process::exit(1);
        }
    };

    let mut todos = load_todos();
    if num > todos.len() {
        eprintln!("{}: no item #{num} (only {} items)", prog_name(), todos.len());
        process::exit(1);
    }

    let idx = num - 1;
    if todos[idx].starts_with("[x] ") {
        println!("Item #{num} is already done.");
        return;
    }
    if let Some(rest) = todos[idx].strip_prefix("[ ] ") {
        todos[idx] = format!("[x] {rest}");
    } else {
        // Non-standard format, just mark it
        todos[idx] = format!("[x] {}", &todos[idx]);
    }
    save_todos(&todos);
    println!("Marked #{num} as done.");
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage();
        return;
    }

    match args[1].as_str() {
        "add" => {
            if args.len() < 3 {
                eprintln!("{}: 'add' requires text argument", prog_name());
                process::exit(1);
            }
            let text = args[2..].join(" ");
            cmd_add(&text);
        }
        "list" => cmd_list(),
        "done" => {
            if args.len() < 3 {
                eprintln!("{}: 'done' requires a number argument", prog_name());
                process::exit(1);
            }
            cmd_done(&args[2]);
        }
        "help" => print_usage(),
        other => {
            eprintln!("{}: unknown command: {other}", prog_name());
            print_usage();
            process::exit(1);
        }
    }
}
