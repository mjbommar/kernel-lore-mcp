"""Shared fixtures for ingest + tool tests.

`make_synthetic_shard` writes a bare git repo that mimics the
public-inbox v2 layout: one commit per message, each commit's tree
holding a single blob named `m` with the raw RFC822 text. This is
what `_core.ingest_shard` expects. Using real `git` here keeps the
contract identical to production — no mock gix layer.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

SAMPLE_MESSAGES: list[bytes] = [
    b"From: Alice <alice@example.com>\r\n"
    b"Subject: [PATCH v3 1/2] ksmbd: tighten ACL bounds\r\n"
    b"Date: Mon, 14 Apr 2026 12:00:00 +0000\r\n"
    b"Message-ID: <m1@x>\r\n"
    b"\r\n"
    b"Prose here explaining the change.\r\n"
    b'Fixes: deadbeef01234567 ("ksmbd: initial ACL handling")\r\n'
    b"Reviewed-by: Carol <carol@example.com>\r\n"
    b"Signed-off-by: Alice <alice@example.com>\r\n"
    b"Cc: stable@vger.kernel.org # 5.15+\r\n"
    b"---\r\n"
    b"diff --git a/fs/smb/server/smbacl.c b/fs/smb/server/smbacl.c\r\n"
    b"--- a/fs/smb/server/smbacl.c\r\n"
    b"+++ b/fs/smb/server/smbacl.c\r\n"
    b"@@ -1,1 +1,2 @@ int smb_check_perm_dacl(struct ksmbd_conn *c)\r\n"
    b" a\r\n"
    b"+b\r\n",
    b"From: Alice <alice@example.com>\r\n"
    b"Subject: [PATCH v3 2/2] ksmbd: follow-up\r\n"
    b"Date: Mon, 14 Apr 2026 12:05:00 +0000\r\n"
    b"Message-ID: <m2@x>\r\n"
    b"In-Reply-To: <m1@x>\r\n"
    b"References: <m1@x>\r\n"
    b"\r\n"
    b"More prose.\r\n"
    b"Signed-off-by: Alice <alice@example.com>\r\n"
    b"---\r\n"
    b"diff --git a/fs/smb/server/smb2pdu.c b/fs/smb/server/smb2pdu.c\r\n"
    b"--- a/fs/smb/server/smb2pdu.c\r\n"
    b"+++ b/fs/smb/server/smb2pdu.c\r\n"
    b"@@ -1,1 +1,2 @@ int smb2_create(struct ksmbd_conn *c)\r\n"
    b" a\r\n"
    b"+b\r\n",
]


def make_synthetic_shard(shard_dir: Path, messages: list[bytes] | None = None) -> Path:
    """Build a bare git repo that mimics a public-inbox shard.

    Returns `shard_dir` for convenience.
    """
    msgs = messages if messages is not None else SAMPLE_MESSAGES
    work = shard_dir.parent / f"{shard_dir.name}-work"
    work.mkdir(parents=True, exist_ok=True)

    env = {
        "GIT_AUTHOR_NAME": "tester",
        "GIT_AUTHOR_EMAIL": "t@e",
        "GIT_COMMITTER_NAME": "tester",
        "GIT_COMMITTER_EMAIL": "t@e",
    }

    def git(*args: str, cwd: Path) -> None:
        subprocess.run(
            ["git", *args],
            cwd=cwd,
            env=env,
            check=True,
            capture_output=True,
        )

    git("init", "-q", "-b", "master", ".", cwd=work)
    for i, msg in enumerate(msgs):
        (work / "m").write_bytes(msg)
        git("add", "m", cwd=work)
        git("commit", "-q", "-m", f"m{i}", cwd=work)

    if shard_dir.exists():
        import shutil

        shutil.rmtree(shard_dir)

    subprocess.run(
        ["git", "clone", "--bare", "-q", str(work), str(shard_dir)],
        check=True,
        capture_output=True,
    )
    return shard_dir
