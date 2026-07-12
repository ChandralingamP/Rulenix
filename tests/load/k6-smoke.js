import http from "k6/http";
import { check, sleep } from "k6";

export const options = {
  thresholds: {
    http_req_failed: ["rate<0.01"],
    http_req_duration: ["p(95)<500"],
  },
  scenarios: {
    health: {
      executor: "constant-vus",
      vus: 5,
      duration: "1m",
    },
  },
};

export default function () {
  const baseUrl = __ENV.RULENIX_BASE_URL || "http://localhost:8080";
  const response = http.get(`${baseUrl}/api/health/ready`);
  check(response, {
    "ready": (value) => value.status === 200,
  });
  sleep(1);
}
