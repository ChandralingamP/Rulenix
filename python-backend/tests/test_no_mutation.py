from pathlib import Path


def test_python_mutation_inventory_zero():
    source = "\n".join(p.read_text(encoding="utf-8") for p in Path("app").rglob("*.py"))
    forbidden = ("angelbroking", "place_order", "modify_order", "cancel_order", "gtt", "oco", "manual_close")
    assert not any(word in source.lower() for word in forbidden)

