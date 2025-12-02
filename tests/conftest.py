"""
Pytest fixtures for git-remote-dokuwiki integration tests.
"""

import os
import subprocess
import tempfile
import time
from pathlib import Path

import pytest
import requests

TESTS_DIR = Path(__file__).parent
DOCKER_COMPOSE_FILE = TESTS_DIR / "docker-compose.yml"
CONFIG_DIR = TESTS_DIR / "config"
WIKI_URL = "http://localhost:8080"


def wait_for_container(timeout=30):
    """Wait for the container to be running."""
    start = time.time()
    while time.time() - start < timeout:
        result = subprocess.run(
            ["docker", "compose", "-f", str(DOCKER_COMPOSE_FILE), "ps", "-q"],
            capture_output=True,
            text=True,
            cwd=TESTS_DIR
        )
        if result.stdout.strip():
            return True
        time.sleep(1)
    return False


def copy_config_files():
    """Copy config files into the running container."""
    container_name = subprocess.run(
        ["docker", "compose", "-f", str(DOCKER_COMPOSE_FILE), "ps", "-q"],
        capture_output=True,
        text=True,
        cwd=TESTS_DIR
    ).stdout.strip()

    if not container_name:
        return False

    # Copy each config file
    for config_file in ["local.php", "users.auth.php", "acl.auth.php"]:
        src = CONFIG_DIR / config_file
        subprocess.run(
            ["docker", "cp", str(src), f"{container_name}:/storage/conf/{config_file}"],
            check=True
        )

    # Fix ownership
    subprocess.run(
        ["docker", "exec", container_name, "chown", "-R", "www-data:www-data", "/storage/conf"],
        check=True
    )

    return True


def wait_for_wiki(timeout=60):
    """Wait for the wiki to be ready."""
    start = time.time()
    while time.time() - start < timeout:
        try:
            resp = requests.get(f"{WIKI_URL}/lib/exe/jsonrpc.php", timeout=2)
            if resp.status_code in (200, 405):  # 405 = method not allowed (but server is up)
                return True
        except requests.RequestException:
            pass
        time.sleep(1)
    return False


def wipe_wiki_data():
    """Wipe all wiki pages and media."""
    # Use JSON-RPC to delete all pages
    # First, get list of all pages
    session = requests.Session()

    # Login as admin
    resp = session.post(
        f"{WIKI_URL}/lib/exe/jsonrpc.php",
        json={"jsonrpc": "2.0", "method": "dokuwiki.login", "params": {"user": "admin", "pass": "admin123"}, "id": 1}
    )

    # Get all pages
    resp = session.post(
        f"{WIKI_URL}/lib/exe/jsonrpc.php",
        json={"jsonrpc": "2.0", "method": "core.listPages", "params": {"namespace": ""}, "id": 2}
    )
    if resp.ok:
        result = resp.json()
        pages = result.get("result", [])
        for page in pages:
            page_id = page.get("id", "")
            if page_id:
                # Delete by saving empty content
                session.post(
                    f"{WIKI_URL}/lib/exe/jsonrpc.php",
                    json={
                        "jsonrpc": "2.0",
                        "method": "core.savePage",
                        "params": {"page": page_id, "text": "", "summary": "test cleanup"},
                        "id": 3
                    }
                )


@pytest.fixture(scope="session")
def docker_wiki():
    """Start the DokuWiki container for the test session."""
    # Start container
    subprocess.run(
        ["docker", "compose", "-f", str(DOCKER_COMPOSE_FILE), "up", "-d"],
        check=True,
        cwd=TESTS_DIR
    )

    # Wait for container to be running
    if not wait_for_container():
        pytest.fail("Container failed to start")

    # Copy config files into container
    copy_config_files()

    # Wait for wiki to be ready
    if not wait_for_wiki():
        # Get logs for debugging
        logs = subprocess.run(
            ["docker", "compose", "-f", str(DOCKER_COMPOSE_FILE), "logs"],
            capture_output=True,
            text=True,
            cwd=TESTS_DIR
        )
        pytest.fail(f"Wiki failed to start. Logs:\n{logs.stdout}\n{logs.stderr}")

    # Wipe any existing data
    wipe_wiki_data()

    yield WIKI_URL

    # Don't stop container - leave it running for inspection if needed
    # To stop: docker compose -f tests/docker-compose.yml down -v


@pytest.fixture
def temp_repo(tmp_path):
    """Create a temporary directory for a git clone."""
    repo_dir = tmp_path / "repo"
    repo_dir.mkdir()
    return repo_dir


@pytest.fixture
def admin_credentials():
    """Admin user credentials."""
    return {"user": "admin", "password": "admin123"}


@pytest.fixture
def limited_credentials():
    """Limited user credentials."""
    return {"user": "limited", "password": "limited123"}


def clone_wiki(repo_dir, user, password, namespace=None, host="localhost:8080"):
    """Clone a wiki namespace to a directory."""
    url = f"dokuwiki::{user}@{host}"
    if namespace:
        url += f"/{namespace}"

    env = os.environ.copy()
    env["DOKUWIKI_PASSWORD"] = password

    result = subprocess.run(
        ["git", "clone", url, str(repo_dir)],
        env=env,
        capture_output=True,
        text=True
    )
    return result


def git_push(repo_dir, password):
    """Push changes from a repo."""
    env = os.environ.copy()
    env["DOKUWIKI_PASSWORD"] = password

    result = subprocess.run(
        ["git", "push"],
        cwd=repo_dir,
        env=env,
        capture_output=True,
        text=True
    )
    return result


def git_pull(repo_dir, password):
    """Pull changes to a repo."""
    env = os.environ.copy()
    env["DOKUWIKI_PASSWORD"] = password

    result = subprocess.run(
        ["git", "pull"],
        cwd=repo_dir,
        env=env,
        capture_output=True,
        text=True
    )
    return result
