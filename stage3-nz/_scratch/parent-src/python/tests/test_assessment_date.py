"""`assessment_date` is a reserved bitemporal field (see docs/bitemporal.md):
it must round-trip through the wire models without changing the request shape
for callers that do not use it."""
from __future__ import annotations

import json
from datetime import date

from axiom_rules_engine.models import (
    Dataset,
    ExecutionQuery,
    ExecutionRequest,
    ExecutionResponse,
    Period,
    Program,
)


def _query(assessment_date: date | None = None) -> ExecutionQuery:
    period = Period(period_kind="Month", start="2026-01-01", end="2026-01-31")
    return ExecutionQuery(
        entity_id="household-1",
        period=period,
        outputs=["adjusted_amount"],
        assessment_date=assessment_date,
    )


def test_assessment_date_round_trips() -> None:
    query = _query(assessment_date=date(2026, 3, 15))
    request = ExecutionRequest(
        mode="explain", program=Program(), dataset=Dataset(), queries=[query]
    )

    # Serialise the way the client does and parse back.
    payload = json.loads(request.model_dump_json(exclude_none=True))
    assert payload["queries"][0]["assessment_date"] == "2026-03-15"
    reparsed = ExecutionRequest.model_validate(payload)
    assert reparsed.queries[0].assessment_date == date(2026, 3, 15)


def test_unset_assessment_date_is_absent_from_the_wire() -> None:
    request = ExecutionRequest(
        mode="explain", program=Program(), dataset=Dataset(), queries=[_query()]
    )
    payload = json.loads(request.model_dump_json(exclude_none=True))
    assert "assessment_date" not in payload["queries"][0]


def test_responses_parse_with_and_without_the_echo() -> None:
    base_result = {
        "entity_id": "household-1",
        "period": {
            "period_kind": "Month",
            "start": "2026-01-01",
            "end": "2026-01-31",
        },
        "outputs": {},
    }
    metadata = {"requested_mode": "explain", "actual_mode": "explain"}

    legacy = ExecutionResponse.model_validate(
        {"metadata": metadata, "results": [base_result]}
    )
    assert legacy.results[0].assessment_date is None

    echoed = ExecutionResponse.model_validate(
        {
            "metadata": metadata,
            "results": [{**base_result, "assessment_date": "2026-03-15"}],
        }
    )
    assert echoed.results[0].assessment_date == date(2026, 3, 15)
