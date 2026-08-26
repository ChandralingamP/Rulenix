/** @vitest-environment jsdom */
import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import AdminEgressIpsPage from "./AdminEgressIpsPage.jsx";
import apiClient from "../utils/axiosConfig.js";

vi.mock("../utils/axiosConfig.js", () => ({
  default: { get: vi.fn(), post: vi.fn() },
}));
vi.mock("react-router-dom", async (importOriginal) => ({
  ...(await importOriginal()),
  useNavigate: () => vi.fn(),
  useOutletContext: () => ({ session: { username: "ADMIN" } }),
}));

const ip = {
  id: "ip-id",
  ip_address: "51.161.140.103",
  configuration_status: "CONFIGURED",
  verification_status: "VERIFIED",
  assigned_user_id: null,
  last_verified_at: "2026-08-26T12:00:00Z",
  status_message: "",
};

describe("AdminEgressIpsPage", () => {
  afterEach(() => { cleanup(); vi.clearAllMocks(); });

  it("shows server, verification, and availability state", async () => {
    apiClient.get.mockResolvedValue({ data: [ip] });
    render(<AdminEgressIpsPage />);
    expect(await screen.findByText("51.161.140.103")).toBeInTheDocument();
    expect(screen.getAllByText("CONFIGURED").length).toBeGreaterThan(0);
    expect(screen.getAllByText("VERIFIED").length).toBeGreaterThan(0);
    expect(screen.getByText("AVAILABLE")).toBeInTheDocument();
  });

  it("registers only through the typed admin endpoint", async () => {
    apiClient.get.mockResolvedValue({ data: [] });
    apiClient.post.mockResolvedValue({ data: { egress_ip: ip } });
    const user = userEvent.setup();
    render(<AdminEgressIpsPage />);
    await user.type(await screen.findByLabelText("Public IPv4 address"), "51.161.140.103");
    await user.click(screen.getByRole("button", { name: "Add and verify" }));
    await waitFor(() => expect(apiClient.post).toHaveBeenCalledWith("/admin/egress-ips", { ip_address: "51.161.140.103" }));
  });
});
