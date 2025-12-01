//! Verbosity control for output messages

use std::env;
use std::sync::atomic::{AtomicU8, Ordering};

/// Global verbosity level (can be updated by git's option command)
static VERBOSITY_LEVEL: AtomicU8 = AtomicU8::new(0);

/// Verbosity level for output messages
#[derive(Clone, Copy)]
pub struct Verbosity;

impl Verbosity {
    /// Create verbosity, initializing from environment variables if not already set
    ///
    /// Checks DOKUWIKI_VERBOSE env var:
    /// - DOKUWIKI_VERBOSE=1 maps to level 2 (info, same as git -v)
    /// - DOKUWIKI_VERBOSE=2 maps to level 3 (debug, same as git -vv)
    pub fn from_env() -> Self {
        let mut level = 0u8;

        // Check tool-specific env var
        if let Ok(v) = env::var("DOKUWIKI_VERBOSE") {
            if let Ok(l) = v.parse::<u8>() {
                // Map DOKUWIKI_VERBOSE=1 to git's verbosity=2 (-v)
                // Map DOKUWIKI_VERBOSE=2 to git's verbosity=3 (-vv)
                level = l + 1;
            }
        }

        VERBOSITY_LEVEL.store(level, Ordering::SeqCst);
        Verbosity
    }

    /// Set verbosity level (called when git sends "option verbosity N")
    pub fn set_level(&self, level: u8) {
        // Only increase verbosity, don't decrease (env var takes precedence)
        let current = VERBOSITY_LEVEL.load(Ordering::SeqCst);
        if level > current {
            VERBOSITY_LEVEL.store(level, Ordering::SeqCst);
        }
    }

    /// Get current verbosity level
    fn level(&self) -> u8 {
        VERBOSITY_LEVEL.load(Ordering::SeqCst)
    }

    /// Print an info message (verbosity >= 2, i.e. git -v)
    pub fn info(&self, msg: &str) {
        if self.level() >= 2 {
            eprintln!("{}", msg);
        }
    }

    /// Print a debug message (verbosity >= 3, i.e. git -vv)
    pub fn debug(&self, msg: &str) {
        if self.level() >= 3 {
            eprintln!("DEBUG: {}", msg);
        }
    }
}
