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

### Shallow clone

For a faster clone without full history, use `--depth`:

```bash
git clone --depth 1 dokuwiki::user@wiki.example.com      # latest revision only
git clone --depth 5 dokuwiki::user@wiki.example.com      # last 5 revisions per page
```

This limits the number of revisions fetched per page/media file, significantly reducing clone time for wikis with long histories.

## Authentication

The tool uses git's credential helper system. On first use, it will prompt for your password and store the session cookie in `.git/dokuwiki-cookies.txt` (Netscape cookie format). This cookie file can be used by other tools that support Netscape cookies, such as `curl -b`.

You can also set the `DOKUWIKI_PASSWORD` environment variable.

To use a cookie file from a different location (e.g., to share authentication between repos):

```bash
DOKUWIKI_COOKIE_FILE=/path/to/cookies.txt git clone dokuwiki::user@wiki.example.com
```

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

- Pages are stored as `.md` files with directory structure matching DokuWiki namespaces
- Media files are stored alongside pages (e.g., `namespace/image.png`)
- Each wiki revision becomes a git commit with the original timestamp and author
- Pushing creates new wiki revisions with the git commit message as the edit summary

## File extension

Pages use the `.md` extension by default. This works well for both DokuWiki syntax and Markdown wikis, as most editors will provide reasonable syntax highlighting for either.

To use a different extension, add `?ext=` to the URL:

```bash
git clone "dokuwiki::user@wiki.example.com?ext=txt"
git clone "dokuwiki::user@wiki.example.com/namespace?ext=dw"
```

The extension is stored in `.git/config` as part of the remote URL, so it persists across operations.

**Note:** Media files are identified as any file that doesn't have the configured page extension. This means `.md` files cannot be used as media attachments when using the default extension.

## Requirements

- DokuWiki with JSON-RPC enabled (API version 14+)
- A user account with appropriate permissions

### Remote API Access

If you get "forbidden" errors, your user may not have API access. In `conf/local.php`, the `remoteuser` setting controls who can use the API:

```php
$conf['remoteuser'] = '@user';        // Allow all logged-in users
```

Options include `@user` (any authenticated user), `@admin` (admins only), or specific usernames.

## License

AGPL-3.0
