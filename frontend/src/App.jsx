import { BrowserRouter, Navigate, useLocation } from "react-router-dom";
import Layout from "./components/Layout.jsx";
import HomePage from "./pages/HomePage.jsx";
import ProfitLossPage from "./pages/ProfitLossPage.jsx";
import LoginPage from "./pages/LoginPage.jsx";
import SignupPage from "./pages/SignupPage.jsx";
import VerifyOtpPage from "./pages/VerifyOtpPage.jsx";
import ForgotPasswordPage from "./pages/ForgotPasswordPage.jsx";
import VerifyResetOtpPage from "./pages/VerifyResetOtpPage.jsx";
import ResetPasswordPage from "./pages/ResetPasswordPage.jsx";
import AdminPage from "./pages/AdminPage.jsx";
import AdminRiskLimitsPage from "./pages/AdminRiskLimitsPage.jsx";
import AdminJobsPage from "./pages/AdminJobsPage.jsx";
import AdminDailyTradesPage from "./pages/AdminDailyTradesPage.jsx";
import LogsViewerPage from "./pages/LogsViewerPage.jsx";
import StrategiesPage from "./pages/StrategiesPage.jsx";
import BacktestingPage from "./pages/BacktestingPage.jsx";

function AppRoutes() {
  const location = useLocation();
  const path = location.pathname.replace(/\/+$/, "") || "/";

  if (path === "/login") return <LoginPage />;
  if (path === "/signup") return <SignupPage />;
  if (path === "/verify-otp") return <VerifyOtpPage />;
  if (path === "/forgot-password") return <ForgotPasswordPage />;
  if (path === "/forgot-password/verify") return <VerifyResetOtpPage />;
  if (path === "/forgot-password/reset") return <ResetPasswordPage />;
  if (path === "/logs") return <LogsViewerPage />;
  if (path === "/admin") return <Navigate to="/admin/users" replace />;

  let page = <HomePage />;
  if (path === "/pnl") page = <ProfitLossPage />;
  else if (path === "/strategies") page = <StrategiesPage />;
  else if (path === "/backtesting") page = <BacktestingPage />;
  else if (path === "/admin/users") page = <AdminPage />;
  else if (path === "/admin/limits") page = <AdminRiskLimitsPage />;
  else if (path === "/admin/jobs") page = <AdminJobsPage />;
  else if (path === "/admin/trades") page = <AdminDailyTradesPage />;
  else if (path !== "/") return <Navigate to="/" replace />;

  return <Layout>{page}</Layout>;
}

export default function App() {
  return (
    <BrowserRouter>
      <AppRoutes />
    </BrowserRouter>
  );
}
