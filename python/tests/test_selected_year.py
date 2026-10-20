"""Actual selected-year CLI execution and explicit native-dense refusal."""
from pathlib import Path

import pytest

from axiom_rules_engine import AxiomRulesEngine, CompiledDenseProgram, CompiledExecutionRequest


@pytest.fixture
def annual_source(tmp_path):
    root = (tmp_path / "rulespec-ca").resolve()
    path = root / "ca/policies/tests/annual.yaml"
    path.parent.mkdir(parents=True)
    fixture = Path(__file__).resolve().parents[2] / "tests/fixtures/selected_year_input.yaml"
    path.write_bytes(fixture.read_bytes())
    return root, path


def test_dense_refuses_selected_year(annual_source):
    root, path = annual_source
    # An unbounded single version reaches the selected-year capability check;
    # the original end-bounded fixture has an independent dense restriction.
    path.write_text(path.read_text().replace("    effective_to: '2026-07-31'\n", ""))
    with pytest.raises(Exception, match="selected-year input requires period-aware Explain"):
        CompiledDenseProgram.from_file(path, rulespec_roots=[root], entity="Person")


def test_cli_preserves_year_person_and_precision(annual_source, tmp_path):
    root, path = annual_source
    engine = AxiomRulesEngine(Path(__file__).resolve().parents[2] / "target/debug/axiom-rules-engine")
    artifact = tmp_path / "annual.json"
    engine.compile(program_path=path, rulespec_roots=(root,), output_path=artifact)
    target = "ca:policies/tests/annual"
    for mode in ("explain", "fast"):
        for year, person, error in [(2025, "p", None), (2026, "p", "missing input"),
                                    (2025, "other", "missing input")]:
            request = CompiledExecutionRequest.model_validate({
                "mode": mode,
                "dataset": {"inputs": [{"name": f"{target}#input.income", "entity": "Person",
                    "entity_id": person, "interval": {"start": f"{year}-01-01", "end": f"{year}-12-31"},
                    "value": {"kind": "decimal", "value": "1234.567890123456789"}}], "relations": []},
                "queries": [{"entity_id": "p", "period": {"period_kind": "month", "start": "2026-07-01",
                    "end": "2026-07-31"}, "outputs": [f"{target}#out"]}]})
            if error:
                with pytest.raises(RuntimeError, match=error):
                    engine.execute_compiled(artifact_path=artifact, request=request)
            else:
                result = engine.execute_compiled(artifact_path=artifact, request=request).model_dump(mode="json")
                assert result["results"][0]["outputs"][f"{target}#out"]["value"]["value"] == "1234.567890123456789"
                assert result["metadata"]["actual_mode"] == "explain"
