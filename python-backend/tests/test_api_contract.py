from fastapi.testclient import TestClient

from app.main import app


def test_liveness_and_deferred_boundary():
    with TestClient(app) as client:
        response = client.get("/api/health/live")
        assert response.status_code == 200
        assert response.json() == {"status": "ok", "service": "Rulenix Rust API"}
        deferred = client.post("/api/home/connect/")
        assert deferred.status_code == 503
        assert deferred.json()["code"] == "python_foundation_deferred"

