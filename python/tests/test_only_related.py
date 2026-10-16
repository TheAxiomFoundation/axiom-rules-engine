"""Compatibility branch: actual CLI projection, explicit native dense refusal."""
from pathlib import Path

import pytest

from axiom_rules_engine import AxiomRulesEngine, CompiledDenseProgram, CompiledExecutionRequest


@pytest.fixture
def source(tmp_path):
    root = (tmp_path / "rulespec-us").resolve()
    path = root / "us/policies/tests/only_related.yaml"
    path.parent.mkdir(parents=True)
    fixture = Path(__file__).resolve().parents[2] / "tests/fixtures/only_related.yaml"
    path.write_bytes(fixture.read_bytes())
    return root, path


def test_dense_explicitly_refuses_compatibility_projection(source):
    root, path = source
    with pytest.raises(Exception, match="only_related uses the explain backend"):
        CompiledDenseProgram.from_file(path, rulespec_roots=[root], entity="Determination")


def test_cli_boolean_projection_and_capability_metadata(source, tmp_path):
    root, path = source
    binary = Path(__file__).resolve().parents[2] / "target/debug/axiom-rules-engine"
    engine = AxiomRulesEngine(binary)
    artifact = tmp_path / "artifact.json"
    engine.compile(program_path=path, rulespec_roots=(root,), output_path=artifact)
    target = "us:policies/tests/only_related"
    interval = {"start": "2026-01-01", "end": "2026-01-31"}
    for mode in ("explain", "fast"):
        for targets, value, error in [(["s"], True, None), (["s"], False, None),
                                       ([], False, "missing related entity"),
                                       (["s1", "s2"], False, "ambiguous relation")]:
            req = CompiledExecutionRequest.model_validate({
                "mode": mode,
                "dataset": {
                    "inputs": [{"name": f"{target}#input.decision", "entity": "Subject", "entity_id": i,
                                "interval": interval, "value": {"kind": "bool", "value": value}} for i in targets],
                    "relations": [{"name": f"{target}#relation.determination_subject", "tuple": ["d", i],
                                   "interval": interval} for i in targets]},
                "queries": [{"entity_id": "d", "period": {"period_kind": "month", **interval},
                             "outputs": [f"{target}#projected"]}]})
            if error:
                with pytest.raises(RuntimeError, match=error):
                    engine.execute_compiled(artifact_path=artifact, request=req)
            else:
                result = engine.execute_compiled(artifact_path=artifact, request=req).model_dump(mode="json")
                assert result["results"][0]["outputs"][f"{target}#projected"]["value"]["value"] is value
                assert result["metadata"]["actual_mode"] == "explain"
                if mode == "fast":
                    assert "only_related" in result["metadata"]["fallback_reason"]
