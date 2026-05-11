from __future__ import annotations

from calendar import monthrange
from datetime import date
from pathlib import Path
from typing import Any

import yaml

from .models import Period


def load_case_list(path: str | Path) -> list[dict[str, Any]]:
    data = yaml.safe_load(Path(path).read_text())
    if isinstance(data, list):
        return data
    if isinstance(data, dict) and isinstance(data.get("cases"), list):
        return data["cases"]
    raise ValueError(f"{path} must contain a case list or a mapping with `cases`")


def coerce_period(value: Any) -> Period:
    if isinstance(value, Period):
        return value
    if isinstance(value, dict):
        return Period.model_validate(value)
    if isinstance(value, int):
        return _tax_year(value)
    if isinstance(value, str):
        if len(value) == 4 and value.isdigit():
            return _tax_year(int(value))
        if len(value) == 7 and value[4] == "-":
            year = int(value[:4])
            month = int(value[5:])
            end_day = monthrange(year, month)[1]
            return Period(
                period_kind="month",
                start=date(year, month, 1),
                end=date(year, month, end_day),
            )
    raise ValueError(f"unsupported period shorthand: {value!r}")


def _tax_year(year: int) -> Period:
    return Period(
        period_kind="tax_year",
        start=date(year, 4, 6),
        end=date(year + 1, 4, 5),
    )
