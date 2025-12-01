//! Git remote helper protocol parsing

/// Commands from git to the remote helper
#[derive(Debug)]
pub enum Command {
    /// Report capabilities
    Capabilities,
    /// List refs
    List,
    /// Import a ref (generate fast-import stream)
    Import(String),
    /// Export (read fast-export stream)
    Export,
    /// Empty line (end of batch)
    Empty,
    /// Unknown command
    Unknown(String),
}

/// Parse a command line from git
pub fn parse_command(line: &str) -> Command {
    let line = line.trim();

    if line.is_empty() {
        return Command::Empty;
    }

    let mut parts = line.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").to_string();

    match cmd {
        "capabilities" => Command::Capabilities,
        "list" => Command::List,
        "import" => Command::Import(arg),
        "export" => Command::Export,
        _ => Command::Unknown(line.to_string()),
    }
}
