import { BrowserRouter, Navigate, Route, Routes } from "react-router-dom";
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
import LogsViewerPage from "./pages/LogsViewerPage.jsx";
import StrategiesPage from "./pages/StrategiesPage.jsx";
import BacktestingPage from "./pages/BacktestingPage.jsx";

export default function App() {
  return (
    <BrowserRouter
      future={{ v7_startTransition: true, v7_relativeSplatPath: true }}
    >
      <Routes>
        <Route path="/login" element={<LoginPage />} />
        <Route path="/signup" element={<SignupPage />} />
        <Route path="/verify-otp" element={<VerifyOtpPage />} />
        <Route path="/forgot-password" element={<ForgotPasswordPage />} />
        <Route
          path="/forgot-password/verify"
          element={<VerifyResetOtpPage />}
        />
        <Route path="/forgot-password/reset" element={<ResetPasswordPage />} />

        <Route element={<Layout />}>
          <Route index element={<HomePage />} />
          <Route path="pnl" element={<ProfitLossPage />} />
          <Route path="strategies" element={<StrategiesPage />} />
          <Route path="backtesting" element={<BacktestingPage />} />
          <Route path="admin" element={<Navigate to="/admin/users" replace />} />
          <Route path="admin/users" element={<AdminPage />} />
          <Route path="admin/limits" element={<AdminRiskLimitsPage />} />
          <Route path="admin/jobs" element={<AdminJobsPage />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Route>

        {/* Standalone log viewer - must be after Layout routes to avoid catch-all */}
        <Route path="/logs" element={<LogsViewerPage />} />
      </Routes>
    </BrowserRouter>
  );
}
