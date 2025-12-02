"""
Test operations within a specific namespace.
Each test uses its own namespace to avoid conflicts.
"""

import subprocess
import time
import uuid

import requests

from conftest import clone_wiki, git_push, git_pull


def unique_namespace():
    """Generate a unique namespace for a test."""
    return f"test_{uuid.uuid4().hex[:8]}"


def create_namespace_page(docker_wiki, namespace, page_name="start", content=None):
    """Create a page in a namespace via API."""
    if content is None:
        content = f"====== {namespace} ======\n\nStart page.\n"

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
                "page": f"{namespace}:{page_name}",
                "text": content,
                "summary": "Create namespace"
            },
            "id": 2
        }
    )


class TestNamespaceOperations:
    """Test operations within namespaces."""

    def test_clone_namespace(self, docker_wiki, temp_repo, admin_credentials):
        """Clone a specific namespace."""
        namespace = unique_namespace()

        # First create a page in the namespace via API (can't clone empty namespace)
        create_namespace_page(docker_wiki, namespace)

        # Clone the namespace
        result = clone_wiki(
            temp_repo,
            admin_credentials["user"],
            admin_credentials["password"],
            namespace=namespace
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

        # Check that the start page exists (without namespace prefix in filename)
        start_page = temp_repo / "start.md"
        assert start_page.exists(), f"Start page not found. Contents: {list(temp_repo.iterdir())}"

    def test_add_page_in_namespace(self, docker_wiki, temp_repo, admin_credentials):
        """Add a page within a namespace."""
        namespace = unique_namespace()

        # First create a page in the namespace via API (can't clone empty namespace)
        create_namespace_page(docker_wiki, namespace)

        # Clone the namespace
        result = clone_wiki(
            temp_repo,
            admin_credentials["user"],
            admin_credentials["password"],
            namespace=namespace
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

        # Create a new page
        page_path = temp_repo / "newpage.md"
        page_path.write_text("====== New Page ======\n\nContent in namespace.\n")

        subprocess.run(["git", "add", "newpage.md"], cwd=temp_repo, check=True)
        subprocess.run(
            ["git", "commit", "-m", "Add page in namespace"],
            cwd=temp_repo,
            check=True
        )

        # Push
        result = git_push(temp_repo, admin_credentials["password"])
        assert result.returncode == 0, f"Push failed: {result.stderr}"

        # Verify via API that page exists with correct namespace
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
        resp = session.post(
            f"{docker_wiki}/lib/exe/jsonrpc.php",
            json={
                "jsonrpc": "2.0",
                "method": "core.getPage",
                "params": {"page": f"{namespace}:newpage"},
                "id": 2
            }
        )
        result = resp.json()
        assert "New Page" in result.get("result", ""), f"Page not found on wiki: {result}"

    def test_nested_namespace(self, docker_wiki, temp_repo, admin_credentials):
        """Test pages in nested namespaces (subdirectories)."""
        namespace = unique_namespace()

        # First create a page in the namespace via API (can't clone empty namespace)
        create_namespace_page(docker_wiki, namespace)

        # Clone the namespace
        result = clone_wiki(
            temp_repo,
            admin_credentials["user"],
            admin_credentials["password"],
            namespace=namespace
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

        # Create a nested page (subdir)
        subdir = temp_repo / "subns"
        subdir.mkdir()
        page_path = subdir / "nested.md"
        page_path.write_text("====== Nested Page ======\n\nIn a sub-namespace.\n")

        subprocess.run(["git", "add", "subns/nested.md"], cwd=temp_repo, check=True)
        subprocess.run(
            ["git", "commit", "-m", "Add nested page"],
            cwd=temp_repo,
            check=True
        )

        # Push
        result = git_push(temp_repo, admin_credentials["password"])
        assert result.returncode == 0, f"Push failed: {result.stderr}"

        # Verify via API
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
        resp = session.post(
            f"{docker_wiki}/lib/exe/jsonrpc.php",
            json={
                "jsonrpc": "2.0",
                "method": "core.getPage",
                "params": {"page": f"{namespace}:subns:nested"},
                "id": 2
            }
        )
        result = resp.json()
        assert "Nested Page" in result.get("result", ""), f"Nested page not found: {result}"

    def test_rename_page(self, docker_wiki, temp_repo, admin_credentials):
        """Test renaming (moving) a page."""
        namespace = unique_namespace()

        # First create a page in the namespace via API
        create_namespace_page(docker_wiki, namespace, "original", "====== Original ======\n\nOriginal content.\n")

        # Clone the namespace
        result = clone_wiki(
            temp_repo,
            admin_credentials["user"],
            admin_credentials["password"],
            namespace=namespace
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

        # Rename the page
        original = temp_repo / "original.md"
        renamed = temp_repo / "renamed.md"
        assert original.exists(), "Original page not found"

        subprocess.run(["git", "mv", "original.md", "renamed.md"], cwd=temp_repo, check=True)
        subprocess.run(
            ["git", "commit", "-m", "Rename page"],
            cwd=temp_repo,
            check=True
        )

        # Push
        result = git_push(temp_repo, admin_credentials["password"])
        assert result.returncode == 0, f"Push failed: {result.stderr}"

        # Verify via API - old page should be deleted, new page should exist
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

        # Check new page exists
        resp = session.post(
            f"{docker_wiki}/lib/exe/jsonrpc.php",
            json={
                "jsonrpc": "2.0",
                "method": "core.getPage",
                "params": {"page": f"{namespace}:renamed"},
                "id": 2
            }
        )
        result = resp.json()
        assert "Original" in result.get("result", ""), f"Renamed page not found: {result}"

        # Check old page is gone (returns empty string for deleted pages)
        resp = session.post(
            f"{docker_wiki}/lib/exe/jsonrpc.php",
            json={
                "jsonrpc": "2.0",
                "method": "core.getPage",
                "params": {"page": f"{namespace}:original"},
                "id": 3
            }
        )
        result = resp.json()
        assert result.get("result", "x") == "", f"Original page should be deleted: {result}"

    def test_media_file(self, docker_wiki, temp_repo, admin_credentials):
        """Test uploading and deleting media files."""
        namespace = unique_namespace()

        # First create a page in the namespace via API (can't clone empty namespace)
        create_namespace_page(docker_wiki, namespace)

        # Clone the namespace
        result = clone_wiki(
            temp_repo,
            admin_credentials["user"],
            admin_credentials["password"],
            namespace=namespace
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

        # Create a minimal PNG file (1x1 transparent pixel)
        # DokuWiki only allows certain file extensions by default
        media_path = temp_repo / "test.png"
        # Minimal valid PNG: 1x1 transparent pixel
        png_data = bytes([
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,  # PNG signature
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,  # IHDR chunk
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,  # 1x1
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,  # 8-bit RGBA
            0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41,  # IDAT chunk
            0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,  # compressed data
            0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,  #
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,  # IEND chunk
            0x42, 0x60, 0x82
        ])
        media_path.write_bytes(png_data)

        subprocess.run(["git", "add", "test.png"], cwd=temp_repo, check=True)
        subprocess.run(
            ["git", "commit", "-m", "Add media file"],
            cwd=temp_repo,
            check=True
        )

        # Push
        result = git_push(temp_repo, admin_credentials["password"])
        assert result.returncode == 0, f"Push failed: {result.stderr}"

        # Delete the media file
        media_path.unlink()
        subprocess.run(["git", "add", "test.png"], cwd=temp_repo, check=True)
        subprocess.run(
            ["git", "commit", "-m", "Delete media file"],
            cwd=temp_repo,
            check=True
        )

        result = git_push(temp_repo, admin_credentials["password"])
        assert result.returncode == 0, f"Push (delete) failed: {result.stderr}"

    def test_clone_deeply_nested_namespaces(self, docker_wiki, temp_repo, admin_credentials):
        """Test cloning a wiki with deeply nested namespaces gets pages at all levels."""
        namespace = unique_namespace()

        # Create pages at multiple nesting levels via API
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

        # Level 0: namespace:start
        session.post(
            f"{docker_wiki}/lib/exe/jsonrpc.php",
            json={
                "jsonrpc": "2.0",
                "method": "core.savePage",
                "params": {
                    "page": f"{namespace}:start",
                    "text": "====== Root ======\n\nRoot level page.\n",
                    "summary": "Create root"
                },
                "id": 2
            }
        )

        # Level 1: namespace:level1:page1
        session.post(
            f"{docker_wiki}/lib/exe/jsonrpc.php",
            json={
                "jsonrpc": "2.0",
                "method": "core.savePage",
                "params": {
                    "page": f"{namespace}:level1:page1",
                    "text": "====== Level 1 ======\n\nFirst level nested.\n",
                    "summary": "Create level1"
                },
                "id": 3
            }
        )

        # Level 2: namespace:level1:level2:page2
        session.post(
            f"{docker_wiki}/lib/exe/jsonrpc.php",
            json={
                "jsonrpc": "2.0",
                "method": "core.savePage",
                "params": {
                    "page": f"{namespace}:level1:level2:page2",
                    "text": "====== Level 2 ======\n\nSecond level nested.\n",
                    "summary": "Create level2"
                },
                "id": 4
            }
        )

        # Level 3: namespace:level1:level2:level3:page3
        session.post(
            f"{docker_wiki}/lib/exe/jsonrpc.php",
            json={
                "jsonrpc": "2.0",
                "method": "core.savePage",
                "params": {
                    "page": f"{namespace}:level1:level2:level3:page3",
                    "text": "====== Level 3 ======\n\nThird level nested.\n",
                    "summary": "Create level3"
                },
                "id": 5
            }
        )

        # Clone the namespace
        result = clone_wiki(
            temp_repo,
            admin_credentials["user"],
            admin_credentials["password"],
            namespace=namespace
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

        # Verify all levels exist
        assert (temp_repo / "start.md").exists(), "Root level page not found"
        assert (temp_repo / "level1" / "page1.md").exists(), "Level 1 page not found"
        assert (temp_repo / "level1" / "level2" / "page2.md").exists(), "Level 2 page not found"
        assert (temp_repo / "level1" / "level2" / "level3" / "page3.md").exists(), "Level 3 page not found"

        # Verify content
        assert "Level 3" in (temp_repo / "level1" / "level2" / "level3" / "page3.md").read_text()

    def test_clone_deeply_nested_namespace_url(self, docker_wiki, temp_repo, admin_credentials):
        """Test cloning a deeply nested namespace via URL (e.g., user@wiki/ns1/ns2/ns3)."""
        # Create a unique base namespace with nested sub-namespaces
        base = unique_namespace()
        nested_ns = f"{base}:level1:level2:level3"

        # Create pages at the target namespace and below via API
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

        # Create page at the nested namespace root
        session.post(
            f"{docker_wiki}/lib/exe/jsonrpc.php",
            json={
                "jsonrpc": "2.0",
                "method": "core.savePage",
                "params": {
                    "page": f"{nested_ns}:start",
                    "text": "====== Nested Root ======\n\nRoot of nested clone.\n",
                    "summary": "Create nested root"
                },
                "id": 2
            }
        )

        # Create a page one level deeper
        session.post(
            f"{docker_wiki}/lib/exe/jsonrpc.php",
            json={
                "jsonrpc": "2.0",
                "method": "core.savePage",
                "params": {
                    "page": f"{nested_ns}:subdir:page",
                    "text": "====== Sub Page ======\n\nPage in subdirectory.\n",
                    "summary": "Create sub page"
                },
                "id": 3
            }
        )

        # Clone the deeply nested namespace using slash-separated path in URL
        # This tests: dokuwiki::user@host/ns1/ns2/ns3/ns4
        namespace_path = f"{base}/level1/level2/level3"
        result = clone_wiki(
            temp_repo,
            admin_credentials["user"],
            admin_credentials["password"],
            namespace=namespace_path
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

        # Verify we got the pages (without namespace prefix in filenames)
        assert (temp_repo / "start.md").exists(), "Nested root page not found"
        assert (temp_repo / "subdir" / "page.md").exists(), "Sub page not found"

        # Verify content
        assert "Nested Root" in (temp_repo / "start.md").read_text()
        assert "Sub Page" in (temp_repo / "subdir" / "page.md").read_text()

    def test_push_conflict_same_file(self, docker_wiki, temp_repo, admin_credentials):
        """Test push fails when remote has updates to the same file we're pushing."""
        namespace = unique_namespace()

        # Create initial page
        create_namespace_page(docker_wiki, namespace, "page1", "====== Page 1 ======\n\nOriginal content.\n")

        # Clone the namespace
        result = clone_wiki(
            temp_repo,
            admin_credentials["user"],
            admin_credentials["password"],
            namespace=namespace
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

        # Modify the page locally
        page_path = temp_repo / "page1.md"
        page_path.write_text("====== Page 1 ======\n\nLocal modification.\n")
        subprocess.run(["git", "add", "page1.md"], cwd=temp_repo, check=True)
        subprocess.run(["git", "commit", "-m", "Local edit"], cwd=temp_repo, check=True)

        # Wait to ensure wiki edit has a later timestamp
        time.sleep(1)

        # Meanwhile, modify the same page on the wiki via API
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
                    "page": f"{namespace}:page1",
                    "text": "====== Page 1 ======\n\nRemote modification.\n",
                    "summary": "Remote edit"
                },
                "id": 2
            }
        )

        # Push should fail because remote has newer changes
        result = git_push(temp_repo, admin_credentials["password"])
        assert result.returncode != 0, f"Push should have failed but succeeded: {result.stdout}"

    def test_push_conflict_different_file(self, docker_wiki, temp_repo, admin_credentials):
        """Test push fails when remote has updates to a different file."""
        namespace = unique_namespace()

        # Create two pages
        create_namespace_page(docker_wiki, namespace, "page1", "====== Page 1 ======\n\nContent.\n")
        create_namespace_page(docker_wiki, namespace, "page2", "====== Page 2 ======\n\nContent.\n")

        # Clone the namespace
        result = clone_wiki(
            temp_repo,
            admin_credentials["user"],
            admin_credentials["password"],
            namespace=namespace
        )
        assert result.returncode == 0, f"Clone failed: {result.stderr}"

        # Modify page1 locally
        page_path = temp_repo / "page1.md"
        page_path.write_text("====== Page 1 ======\n\nLocal modification.\n")
        subprocess.run(["git", "add", "page1.md"], cwd=temp_repo, check=True)
        subprocess.run(["git", "commit", "-m", "Local edit to page1"], cwd=temp_repo, check=True)

        # Wait to ensure wiki edit has a later timestamp
        time.sleep(1)

        # Meanwhile, modify page2 on the wiki via API
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
                    "page": f"{namespace}:page2",
                    "text": "====== Page 2 ======\n\nRemote modification.\n",
                    "summary": "Remote edit to page2"
                },
                "id": 2
            }
        )

        # Push should fail because remote has newer changes (even to different file)
        result = git_push(temp_repo, admin_credentials["password"])
        assert result.returncode != 0, f"Push should have failed but succeeded: {result.stdout}"
