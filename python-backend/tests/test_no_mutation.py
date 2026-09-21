from pathlib import Path


def test_python_mutation_paths_are_confined_to_typed_angel_transport():
    forbidden = ("/placeorder", "/modifyorder", "/cancelorder", "/gtt/v1/createrule", "/gtt/v1/modifyrule", "/gtt/v1/cancelrule")
    offenders = []
    for path in Path("app").rglob("*.py"):
        source = path.read_text(encoding="utf-8").lower()
        if any(word in source for word in forbidden) and path.as_posix() != "app/broker/angel/rest.py":
            offenders.append(path.as_posix())
    assert offenders == []
