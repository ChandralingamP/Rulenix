/** @vitest-environment jsdom */
import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, cleanup, waitFor } from "@testing-library/react";
import { Provider } from "react-redux";
import axios from "axios";
import App from "./App.jsx";
import { store } from "./app/store";
import { clearAuthUsername, setAuthUsername } from "./utils/authCookies.js";

vi.mock("axios", () => {
  const client = {
    get: vi.fn(),
    post: vi.fn(),
    patch: vi.fn(),
    delete: vi.fn(),
    interceptors: {
      request: { use: vi.fn() },
      response: { use: vi.fn() },
    },
  };
  return { default: { ...client, create: vi.fn(() => client) } };
});

describe("App", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    clearAuthUsername();
    window.history.pushState({}, "", "/");
  });

  it("renders home page header", async () => {
    axios.get.mockImplementation((url) => {
      if (url === "/auth/access/") {
        return Promise.resolve({
          data: {
            username: "TRADER01",
            permissions: { administer_users: false, live_trading: false },
            trading_mode: "demo",
          },
        });
      }
      if (url === "/account/balance") {
        return Promise.resolve({ data: { mode: "demo", balance: 200000 } });
      }
      return Promise.resolve({
        data: {
          client_id: "TRADER01",
          api_key_configured: true,
        },
      });
    });

    setAuthUsername("TRADER01");

    render(
      <Provider store={store}>
        <App />
      </Provider>
    );

    expect(
      await screen.findByText(/Brokerage Connection Overview/i)
    ).toBeInTheDocument();
  });

  it("shows a persistent reconnect warning for an invalid broker token", async () => {
    axios.get.mockImplementation((url) => {
      if (url === "/auth/access/") {
        return Promise.resolve({
          data: {
            username: "TRADER01",
            permissions: { administer_users: false, live_trading: true },
            trading_mode: "demo",
          },
        });
      }
      if (url === "/account/balance") {
        return Promise.resolve({ data: { mode: "demo", balance: 200000 } });
      }
      return Promise.resolve({
        data: {
          client_id: "TRADER01",
          api_key_configured: true,
          connection_state: "invalid",
          connection_message:
            "Angel One API token is invalid or expired. Please establish the broker connection again.",
        },
      });
    });

    setAuthUsername("TRADER01");
    render(
      <Provider store={store}>
        <App />
      </Provider>
    );

    const warning = await screen.findByRole("alert");
    expect(warning).toHaveTextContent("Angel One API token is invalid");
    expect(
      screen.getByRole("link", { name: "Establish broker connection" })
    ).toHaveAttribute("href", "/#broker-connection");
  });

  it("redirects administrators into an admin-only user workspace", async () => {
    axios.get.mockImplementation((url) => {
      if (url === "/auth/access/") {
        return Promise.resolve({
          data: {
            username: "ADMIN01",
            permissions: {
              administer_users: true,
              live_trading: false,
              backtesting: false,
            },
            trading_mode: "demo",
          },
        });
      }
      if (url === "/auth/admin/users/") {
        return Promise.resolve({
          data: [
            {
              id: "admin-id",
              username: "ADMIN01",
              email: "admin@example.com",
              can_administer: true,
              can_live_trade: false,
              can_backtest: false,
              trading_mode: "demo",
            },
          ],
        });
      }
      return Promise.resolve({ data: {} });
    });
    setAuthUsername("ADMIN01");
    window.history.pushState({}, "", "/");

    render(
      <Provider store={store}>
        <App />
      </Provider>
    );

    expect(await screen.findByRole("heading", { name: "User control" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Risk limits" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "System jobs" })).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Strategies" })).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Profit & Loss" })).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Account settings")).not.toBeInTheDocument();
    expect(axios.get.mock.calls.some(([url]) => url === "/account/balance")).toBe(false);
  });

  it("shows all global and per-user limits on the dedicated limits route", async () => {
    const globalLimits = {
      max_lots: 20,
      max_quantity: 10000,
      max_notional: 100000000,
      max_open_positions: 20,
      max_trades_per_day: 100,
      max_daily_realized_loss: 1000000,
      max_daily_unrealized_loss: 1000000,
      max_price_age_seconds: 30,
      margin_requirement_percent: 10,
    };
    axios.get.mockImplementation((url) => {
      if (url === "/auth/access/") {
        return Promise.resolve({
          data: {
            username: "ADMIN01",
            permissions: { administer_users: true },
            trading_mode: "demo",
          },
        });
      }
      if (url === "/risk/admin") {
        return Promise.resolve({
          data: {
            global_limits: globalLimits,
            global_kill_switch: { enabled: false },
            users: [
              {
                id: "trader-id",
                username: "TRADER01",
                limits: null,
                kill_switch: { enabled: false },
              },
            ],
          },
        });
      }
      return Promise.resolve({ data: {} });
    });
    setAuthUsername("ADMIN01");
    window.history.pushState({}, "", "/admin/limits");

    render(
      <Provider store={store}>
        <App />
      </Provider>
    );

    expect(await screen.findByRole("heading", { name: "Risk limits" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Global limits" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Per-user limits" })).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.getByLabelText("Global Maximum lots")).toHaveValue(20);
      expect(screen.getByLabelText("TRADER01 Maximum lots")).toHaveValue(20);
    });
  });
});
