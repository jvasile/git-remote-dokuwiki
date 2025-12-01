# git-remote-dokuwiki

A git remote helper that allows you to use git to interact with a DokuWiki.  You
can fetch, edit, and push, just like a real git repo.

## Installation

```bash
cargo install --path .
```

The binary must be named `git-remote-dokuwiki` and be in your PATH for git to find it.

## Usage

### Clone a wiki

```bash
git clone dokuwiki::user@wiki.example.com
```

### Clone a specific namespace

```bash
git clone dokuwiki::user@wiki.example.com/namespace
```

### Push changes

```bash
# Edit files locally
git add -A
git commit -m "Update start page"
git push
```

### Pull changes

```bash
git pull
```

## Authentication

The tool uses git's credential helper system. On first use, it will prompt for your password and store the session cookie in `.git/dokuwiki-cookies.json`.

You can also set the `DOKUWIKI_PASSWORD` environment variable.

## Verbose output

Use git's `-v` flag for progress info, or `-vv` for debug output:

```bash
git fetch -v
git push -vv
```

Or set the environment variable:

```bash
DOKUWIKI_VERBOSE=1 git fetch  # same as -v
DOKUWIKI_VERBOSE=2 git fetch  # same as -vv
```

## How it works

- Pages are stored as `.txt` files with directory structure matching DokuWiki namespaces
- Each wiki revision becomes a git commit with the original timestamp and author
- Pushing creates new wiki revisions with the git commit message as the edit summary

## Requirements

- DokuWiki with XML-RPC enabled (`lib/exe/xmlrpc.php`)
- A user account with appropriate permissions

## License

AGPL-3.0
