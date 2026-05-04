from __future__ import annotations

import hashlib
import sys
from pathlib import Path
from typing import Any

import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "python"))

from axiom_rules.cli import main
from axiom_rules.source_registry import (
    R2ObjectRef,
    build_r2_client_from_env,
    default_r2_path,
    discover_source_files,
    parse_r2_path,
    source_id_for,
    source_path_for,
    validate_source_registries,
)

SHA_RAW = "a" * 64
SHA_TEXT = "c" * 64


class FakeBody:
    def __init__(self, data: bytes) -> None:
        self.data = data
        self.offset = 0
        self.closed = False

    def read(self, size: int = -1) -> bytes:
        if size < 0:
            size = len(self.data) - self.offset
        chunk = self.data[self.offset : self.offset + size]
        self.offset += len(chunk)
        return chunk

    def close(self) -> None:
        self.closed = True


class FakeR2Error(Exception):
    def __init__(self, code: str) -> None:
        super().__init__(code)
        self.response = {"Error": {"Code": code}}


class FakeR2Client:
    def __init__(self, objects: dict[tuple[str, str], bytes]) -> None:
        self.objects = objects

    def head_object(self, *, Bucket: str, Key: str) -> dict[str, Any]:
        if (Bucket, Key) not in self.objects:
            raise FakeR2Error("NoSuchKey")
        return {}

    def get_object(self, *, Bucket: str, Key: str) -> dict[str, Any]:
        try:
            data = self.objects[(Bucket, Key)]
        except KeyError as error:
            raise FakeR2Error("NoSuchKey") from error
        return {"Body": FakeBody(data)}


def write_source(root: Path, relative: str, body: str) -> Path:
    path = root / "sources" / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)
    return path


def valid_default_body(extra: str = "") -> str:
    return f"""
publisher: Tennessee DHS
canonical_url: https://example.test/manual
retrieved_at: 2026-04-25T00:00:00Z
hashes:
  raw_sha256: {SHA_RAW}
  text_sha256: {SHA_TEXT}
{extra}
"""


def body_with_hashes(*, raw: bytes, text: bytes, extra: str = "") -> str:
    return f"""
publisher: Tennessee DHS
canonical_url: https://example.test/manual
retrieved_at: 2026-04-25T00:00:00Z
hashes:
  raw_sha256: {hashlib.sha256(raw).hexdigest()}
  text_sha256: {hashlib.sha256(text).hexdigest()}
{extra}
"""


def test_derives_source_identity_and_default_r2_paths(tmp_path: Path) -> None:
    root = tmp_path / "us-tn"
    source = write_source(
        root,
        "policies/dhs/snap/manual/23/L.yaml",
        valid_default_body(),
    )

    report = validate_source_registries(root)

    assert report.ok
    assert discover_source_files(root) == [source]
    assert source_path_for(root, source) == "policies/dhs/snap/manual/23/L"
    assert source_id_for(root, source) == "us-tn:policies/dhs/snap/manual/23/L"
    entry = report.entries[0]
    assert entry.source_id == "us-tn:policies/dhs/snap/manual/23/L"
    assert [artifact.name for artifact in entry.artifacts] == ["raw", "text"]
    assert [artifact.r2_path for artifact in entry.artifacts] == [
        "r2://axiom-sources/us-tn/policies/dhs/snap/manual/23/L/raw",
        "r2://axiom-sources/us-tn/policies/dhs/snap/manual/23/L/text",
    ]


def test_repo_and_bucket_can_be_overridden(tmp_path: Path) -> None:
    root = tmp_path / "checkout"
    source = write_source(root, "regulations/7-cfr/273/9.yaml", valid_default_body())

    report = validate_source_registries(root, repo="us", bucket="test-bucket")

    assert report.ok
    assert source_id_for(root, source, repo="us") == "us:regulations/7-cfr/273/9"
    assert report.entries[0].artifacts[0].r2_path == (
        "r2://test-bucket/us/regulations/7-cfr/273/9/raw"
    )
    assert default_r2_path(
        repo="us",
        source_path="regulations/7-cfr/273/9",
        artifact="raw",
        bucket="test-bucket",
    ) == "r2://test-bucket/us/regulations/7-cfr/273/9/raw"


def test_parse_r2_path() -> None:
    assert parse_r2_path("r2://axiom-sources/us/foo/raw") == R2ObjectRef(
        bucket="axiom-sources",
        key="us/foo/raw",
    )
    with pytest.raises(ValueError):
        parse_r2_path("https://example.test/us/foo/raw")


def test_explicit_artifacts_replace_default_hashes(tmp_path: Path) -> None:
    root = tmp_path / "us-tn"
    write_source(
        root,
        "policies/dhs/snap/manual/23/L.yaml",
        f"""
publisher: Tennessee DHS
canonical_url: https://example.test/manual
retrieved_at: 2026-04-25T00:00:00Z
artifacts:
  raw:
    path: manual.pdf
    sha256: {SHA_RAW}
    media_type: application/pdf
  text:
    storage: manual.txt
    sha256: {SHA_TEXT}
    media_type: text/plain
""",
    )

    report = validate_source_registries(root)

    assert report.ok
    assert [artifact.r2_path for artifact in report.entries[0].artifacts] == [
        "r2://axiom-sources/us-tn/policies/dhs/snap/manual/23/L/manual.pdf",
        "r2://axiom-sources/us-tn/policies/dhs/snap/manual/23/L/manual.txt",
    ]


def test_rejects_explicit_akn_artifact(tmp_path: Path) -> None:
    root = tmp_path / "us-tn"
    write_source(
        root,
        "policies/dhs/snap/manual/23/L.yaml",
        f"""
publisher: Tennessee DHS
canonical_url: https://example.test/manual
retrieved_at: 2026-04-25T00:00:00Z
artifacts:
  akn:
    path: akn.xml
    sha256: {SHA_TEXT}
    media_type: application/xml
""",
    )

    report = validate_source_registries(root)

    assert not report.ok
    assert any("`artifacts.akn` is not allowed" in issue.message for issue in report.issues)


def test_rejects_redundant_identity_storage_bad_hash_and_relative_edges(
    tmp_path: Path,
) -> None:
    root = tmp_path / "us-tn"
    write_source(
        root,
        "policies/dhs/snap/manual/23/L.yaml",
        """
id: us-tn:policies/dhs/snap/manual/23/L
publisher: Tennessee DHS
canonical_url: https://example.test/manual
retrieved_at: 2026-04-25T00:00:00Z
storage: r2://axiom-sources/us-tn/policies/dhs/snap/manual/23/L
sets:
  - statutes/7/2014/e/6/A
hashes:
  raw_sha256: not-a-hash
""",
    )

    report = validate_source_registries(root)
    messages = [issue.message for issue in report.issues]

    assert not report.ok
    assert any("`id:` is redundant" in message for message in messages)
    assert any("`storage:` is redundant at top level" in message for message in messages)
    assert any("`sets:` target" in message for message in messages)
    assert any("hashes.raw_sha256" in message for message in messages)
    assert any("hashes.text_sha256" in message for message in messages)


def test_rejects_source_paths_outside_taxonomy(tmp_path: Path) -> None:
    root = tmp_path / "us"
    write_source(root, "other/manual.yaml", valid_default_body())

    report = validate_source_registries(root)

    assert not report.ok
    assert any("source path must start with" in issue.message for issue in report.issues)


def test_rejects_hashes_and_artifacts_together(tmp_path: Path) -> None:
    root = tmp_path / "us"
    write_source(
        root,
        "statutes/7/2014/e/6/A.yaml",
        valid_default_body(
            extra=f"""
artifacts:
  raw:
    path: raw.pdf
    sha256: {SHA_RAW}
"""
        ),
    )

    report = validate_source_registries(root)

    assert not report.ok
    assert any("use either default `hashes:`" in issue.message for issue in report.issues)


def test_verify_r2_accepts_matching_objects(tmp_path: Path) -> None:
    root = tmp_path / "us-tn"
    raw = b"raw pdf bytes"
    text = b"manual text"
    write_source(
        root,
        "policies/dhs/snap/manual/23/L.yaml",
        body_with_hashes(raw=raw, text=text),
    )
    client = FakeR2Client(
        {
            ("axiom-sources", "us-tn/policies/dhs/snap/manual/23/L/raw"): raw,
            ("axiom-sources", "us-tn/policies/dhs/snap/manual/23/L/text"): text,
        }
    )

    report = validate_source_registries(root, verify_r2=True, r2_client=client)

    assert report.ok


def test_verify_r2_reports_missing_and_hash_mismatch(tmp_path: Path) -> None:
    root = tmp_path / "us-tn"
    raw = b"expected raw"
    text = b"expected text"
    write_source(
        root,
        "policies/dhs/snap/manual/23/L.yaml",
        body_with_hashes(raw=raw, text=text),
    )
    client = FakeR2Client(
        {
            ("axiom-sources", "us-tn/policies/dhs/snap/manual/23/L/raw"): b"wrong raw",
        }
    )

    report = validate_source_registries(root, verify_r2=True, r2_client=client)
    messages = [issue.message for issue in report.issues]

    assert not report.ok
    assert any("SHA-256" in message and "does not match" in message for message in messages)
    assert any("text` is missing or inaccessible" in message for message in messages)


def test_verify_r2_requires_client_or_environment(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    root = tmp_path / "us"
    write_source(root, "statutes/7/2014/e/6/A.yaml", valid_default_body())
    for name in (
        "AXIOM_R2_ENDPOINT_URL",
        "AXIOM_R2_ACCOUNT_ID",
        "CLOUDFLARE_R2_ENDPOINT_URL",
        "CLOUDFLARE_R2_ACCOUNT_ID",
        "CLOUDFLARE_ACCOUNT_ID",
        "AXIOM_R2_ACCESS_KEY_ID",
        "CLOUDFLARE_R2_ACCESS_KEY_ID",
        "AWS_ACCESS_KEY_ID",
        "AXIOM_R2_SECRET_ACCESS_KEY",
        "CLOUDFLARE_R2_SECRET_ACCESS_KEY",
        "AWS_SECRET_ACCESS_KEY",
    ):
        monkeypatch.delenv(name, raising=False)

    with pytest.raises(RuntimeError, match="R2 verification requires"):
        validate_source_registries(root, verify_r2=True)


def test_build_r2_client_from_env_reports_missing_values() -> None:
    with pytest.raises(RuntimeError, match="AXIOM_R2_ENDPOINT_URL"):
        build_r2_client_from_env({})


def test_missing_root_is_an_error(tmp_path: Path) -> None:
    report = validate_source_registries(tmp_path / "missing")

    assert not report.ok
    assert report.issues[0].message == "jurisdiction repository root not found"


def test_cli_check_sources_reports_failures(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    root = tmp_path / "us"
    write_source(
        root,
        "statutes/7/2014/e/6/A.yaml",
        """
publisher: USDA
canonical_url: https://example.test/statute
retrieved_at: 2026-04-25T00:00:00Z
hashes:
  raw_sha256: bad
""",
    )

    rc = main(["check-sources", str(root)])
    captured = capsys.readouterr()

    assert rc == 1
    assert "[FAIL]" in captured.out
    assert "Source registry check failed" in captured.out


def test_cli_check_sources_verbose_success(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    root = tmp_path / "us"
    write_source(root, "statutes/7/2014/e/6/A.yaml", valid_default_body())

    rc = main(["check-sources", str(root), "--verbose"])
    captured = capsys.readouterr()

    assert rc == 0
    assert "us:statutes/7/2014/e/6/A" in captured.out
    assert "Validated 1 source registry file" in captured.out


def test_cli_check_sources_verify_r2_reports_missing_config(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    root = tmp_path / "us"
    write_source(root, "statutes/7/2014/e/6/A.yaml", valid_default_body())
    for name in (
        "AXIOM_R2_ENDPOINT_URL",
        "AXIOM_R2_ACCOUNT_ID",
        "CLOUDFLARE_R2_ENDPOINT_URL",
        "CLOUDFLARE_R2_ACCOUNT_ID",
        "CLOUDFLARE_ACCOUNT_ID",
        "AXIOM_R2_ACCESS_KEY_ID",
        "CLOUDFLARE_R2_ACCESS_KEY_ID",
        "AWS_ACCESS_KEY_ID",
        "AXIOM_R2_SECRET_ACCESS_KEY",
        "CLOUDFLARE_R2_SECRET_ACCESS_KEY",
        "AWS_SECRET_ACCESS_KEY",
    ):
        monkeypatch.delenv(name, raising=False)

    rc = main(["check-sources", str(root), "--verify-r2"])
    captured = capsys.readouterr()

    assert rc == 2
    assert "R2 verification requires" in captured.err
