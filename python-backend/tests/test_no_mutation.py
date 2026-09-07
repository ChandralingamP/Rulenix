from pathlib import Path


def test_python_mutation_inventory_zero():
    source = "\n".join(p.read_text(encoding="utf-8") for p in Path("app").rglob("*.py"))
    # Read-only endpoint names and typed guard methods are allowed; mutation HTTP paths are not.
    forbidden = ("/placeorder", "/modifyorder", "/cancelorder", "/gtt/v1/createrule", "/gtt/v1/modifyrule", "/gtt/v1/cancelrule")
    assert not any(word in source.lower() for word in forbidden)
