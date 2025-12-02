"""
Test clone/add/delete/modify/push at the root level.
"""

import subprocess
import time
import uuid
from pathlib import Path

import requests

from conftest import clone_wiki, git_push, git_pull


class TestRootOperations:
    """Test operations at the wiki root (no namespace)."""

    def test_clone_empty_wiki(self, docker_wiki, temp_repo, admin_credentials):
        """Clone an empty wiki."""
        result = clone_wiki(
            temp_repo,
            admin_credentials["user"],
            admin_credentials["password"]
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"
        assert (temp_repo / ".git").is_dir(), "No .git directory created"

    def test_add_page(self, docker_wiki, temp_repo, admin_credentials):
        """Add a new page and push it."""
        # Clone
        result = clone_wiki(
            temp_repo,
            admin_credentials["user"],
            admin_credentials["password"]
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

        # Create a new page
        page_path = temp_repo / "testpage.md"
        page_path.write_text("====== Test Page ======\n\nThis is a test page.\n")

        # Git add and commit
        subprocess.run(["git", "add", "testpage.md"], cwd=temp_repo, check=True)
        subprocess.run(
            ["git", "commit", "-m", "Add test page"],
            cwd=temp_repo,
            check=True
        )

        # Push
        result = git_push(temp_repo, admin_credentials["password"])
        assert result.returncode == 0, f"Push failed: {result.stderr}"

    def test_modify_page(self, docker_wiki, temp_repo, admin_credentials):
        """Modify an existing page and push it."""
        # Clone (should have the page from previous test)
        result = clone_wiki(
            temp_repo,
            admin_credentials["user"],
            admin_credentials["password"]
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

        # Check that testpage exists
        page_path = temp_repo / "testpage.md"
        if not page_path.exists():
            # Create it first
            page_path.write_text("====== Test Page ======\n\nOriginal content.\n")
            subprocess.run(["git", "add", "testpage.md"], cwd=temp_repo, check=True)
            subprocess.run(["git", "commit", "-m", "Add test page"], cwd=temp_repo, check=True)
            git_push(temp_repo, admin_credentials["password"])

        # Modify the page
        page_path.write_text("====== Test Page ======\n\nModified content.\n")

        subprocess.run(["git", "add", "testpage.md"], cwd=temp_repo, check=True)
        subprocess.run(
            ["git", "commit", "-m", "Modify test page"],
            cwd=temp_repo,
            check=True
        )

        # Push
        result = git_push(temp_repo, admin_credentials["password"])
        assert result.returncode == 0, f"Push failed: {result.stderr}"

    def test_delete_page(self, docker_wiki, temp_repo, admin_credentials):
        """Delete a page and push the deletion."""
        # Clone
        result = clone_wiki(
            temp_repo,
            admin_credentials["user"],
            admin_credentials["password"]
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

        # Ensure we have a page to delete
        page_path = temp_repo / "deleteme.md"
        page_path.write_text("====== Delete Me ======\n\nThis page will be deleted.\n")
        subprocess.run(["git", "add", "deleteme.md"], cwd=temp_repo, check=True)
        subprocess.run(["git", "commit", "-m", "Add page to delete"], cwd=temp_repo, check=True)
        result = git_push(temp_repo, admin_credentials["password"])
        assert result.returncode == 0, f"Push (add) failed: {result.stderr}"

        # Delete the page
        page_path.unlink()
        subprocess.run(["git", "add", "deleteme.md"], cwd=temp_repo, check=True)
        subprocess.run(
            ["git", "commit", "-m", "Delete page"],
            cwd=temp_repo,
            check=True
        )

        # Push deletion
        result = git_push(temp_repo, admin_credentials["password"])
        assert result.returncode == 0, f"Push (delete) failed: {result.stderr}"

    def test_pull_changes(self, docker_wiki, tmp_path, admin_credentials):
        """Pull changes made on the wiki."""
        # First clone
        repo1 = tmp_path / "repo1"
        repo1.mkdir()
        result = clone_wiki(
            repo1,
            admin_credentials["user"],
            admin_credentials["password"]
        )
        assert result.returncode == 0, f"Clone 1 failed: {result.stderr}"

        # Wait a moment to ensure the clone's timestamp is in the past
        time.sleep(1)

        # Make a change directly on the wiki via API with a unique page name
        page_name = f"remotepage_{uuid.uuid4().hex[:8]}"
        session = requests.Session()
        session.post(
            f"{docker_wiki}/lib/exe/jsonrpc.php",
            json={
                "jsonrpc": "2.0",
                "method": "dokuwiki.login",
                "params": {"user": "admin", "pass": "admin123"},
                "id": 1
            }
        )
        session.post(
            f"{docker_wiki}/lib/exe/jsonrpc.php",
            json={
                "jsonrpc": "2.0",
                "method": "core.savePage",
                "params": {
                    "page": page_name,
                    "text": "====== Remote Page ======\n\nCreated via API.\n",
                    "summary": "API creation"
                },
                "id": 2
            }
        )

        # Pull changes
        result = git_pull(repo1, admin_credentials["password"])
        assert result.returncode == 0, f"Pull failed: {result.stderr}"

        # Check the file exists
        remote_page = repo1 / f"{page_name}.md"
        assert remote_page.exists(), f"Remote page not pulled. Contents: {list(repo1.iterdir())}"
        assert "Remote Page" in remote_page.read_text()

    def test_shallow_clone(self, docker_wiki, temp_repo, admin_credentials):
        """Test that --depth=1 limits the number of commits."""
        # Create a page with multiple revisions via API
        page_name = f"shallowtest_{uuid.uuid4().hex[:8]}"
        session = requests.Session()
        session.post(
            f"{docker_wiki}/lib/exe/jsonrpc.php",
            json={
                "jsonrpc": "2.0",
                "method": "dokuwiki.login",
                "params": {"user": "admin", "pass": "admin123"},
                "id": 1
            }
        )

        # Create multiple revisions
        for i in range(3):
            time.sleep(1)  # Ensure different timestamps
            session.post(
                f"{docker_wiki}/lib/exe/jsonrpc.php",
                json={
                    "jsonrpc": "2.0",
                    "method": "core.savePage",
                    "params": {
                        "page": page_name,
                        "text": f"====== Shallow Test ======\n\nRevision {i + 1}.\n",
                        "summary": f"Revision {i + 1}"
                    },
                    "id": i + 2
                }
            )

        # Clone with depth=1
        result = clone_wiki(
            temp_repo,
            admin_credentials["user"],
            admin_credentials["password"],
            depth=1
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

        # Check that the page exists with latest content
        page_path = temp_repo / f"{page_name}.md"
        assert page_path.exists(), f"Page not found: {list(temp_repo.iterdir())}"
        assert "Revision 3" in page_path.read_text()

        # Check that we only have 1 commit for this page's history
        log_result = subprocess.run(
            ["git", "log", "--oneline", "--", f"{page_name}.md"],
            cwd=temp_repo,
            capture_output=True,
            text=True
        )
        commits = [line for line in log_result.stdout.strip().split('\n') if line]
        assert len(commits) == 1, f"Expected 1 commit, got {len(commits)}: {commits}"
