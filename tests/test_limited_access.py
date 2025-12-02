"""
Test operations with a user that has limited access.
The 'limited' user can only edit in the 'limited' namespace.
"""

import subprocess

from conftest import clone_wiki, git_push


class TestLimitedAccess:
    """Test operations with limited user permissions."""

    def test_clone_allowed_namespace(self, docker_wiki, temp_repo, limited_credentials):
        """Limited user can clone their allowed namespace."""
        # First create content in the limited namespace as admin
        import requests
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
                    "page": "limited:start",
                    "text": "====== Limited Namespace ======\n\nStart page.\n",
                    "summary": "Setup"
                },
                "id": 2
            }
        )

        # Clone as limited user
        result = clone_wiki(
            temp_repo,
            limited_credentials["user"],
            limited_credentials["password"],
            namespace="limited"
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

    def test_push_to_allowed_namespace(self, docker_wiki, temp_repo, limited_credentials):
        """Limited user can push to their allowed namespace."""
        # Clone the limited namespace
        result = clone_wiki(
            temp_repo,
            limited_credentials["user"],
            limited_credentials["password"],
            namespace="limited"
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

        # Create a new page
        page_path = temp_repo / "mypage.md"
        page_path.write_text("====== My Page ======\n\nLimited user's page.\n")

        subprocess.run(["git", "add", "mypage.md"], cwd=temp_repo, check=True)
        subprocess.run(
            ["git", "commit", "-m", "Add page as limited user"],
            cwd=temp_repo,
            check=True
        )

        # Push should succeed
        result = git_push(temp_repo, limited_credentials["password"])
        assert result.returncode == 0, f"Push failed: {result.stderr}"

    def test_push_to_forbidden_namespace_fails(self, docker_wiki, temp_repo, limited_credentials, admin_credentials):
        """Limited user cannot push to a namespace they don't have access to."""
        # First, create content in a forbidden namespace as admin
        import requests
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
                    "page": "forbidden:start",
                    "text": "====== Forbidden ======\n\nAdmin only.\n",
                    "summary": "Setup"
                },
                "id": 2
            }
        )

        # Clone as limited user (should work - read access)
        result = clone_wiki(
            temp_repo,
            limited_credentials["user"],
            limited_credentials["password"],
            namespace="forbidden"
        )
        # Clone might succeed (read access) or fail depending on ACL
        # Let's try to push regardless

        if result.returncode == 0:
            # Try to modify
            page_path = temp_repo / "start.md"
            if page_path.exists():
                page_path.write_text("====== Forbidden ======\n\nHacked!\n")
            else:
                page_path.write_text("====== New Page ======\n\nShould fail.\n")

            subprocess.run(["git", "add", "-A"], cwd=temp_repo, check=True)
            subprocess.run(
                ["git", "commit", "-m", "Try to modify forbidden namespace"],
                cwd=temp_repo,
                check=True
            )

            # Push should fail
            result = git_push(temp_repo, limited_credentials["password"])
            assert result.returncode != 0, "Push to forbidden namespace should have failed"

    def test_clone_root_with_limited_access(self, docker_wiki, temp_repo, limited_credentials):
        """Limited user can clone root but only sees readable content."""
        result = clone_wiki(
            temp_repo,
            limited_credentials["user"],
            limited_credentials["password"]
        )
        # This should succeed - user has read access to root
        assert result.returncode == 0, f"Clone failed: {result.stderr}"
