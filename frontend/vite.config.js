import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const frontendPort = Number(process.env.RULENIX_FRONTEND_PORT || 5173);
const backendUrl = process.env.RULENIX_BACKEND_URL || "http://localhost:8080";

export default defineConfig({
  plugins: [react()],
  server: {
    host: "0.0.0.0",
    port: frontendPort,
    allowedHosts: [".trycloudflare.com"],
    proxy: {
      "/api": {
        target: backendUrl,
        changeOrigin: true,
        ws: true,
      },
    },
  },
  preview: {
    host: "0.0.0.0",
    port: 4173,
  },
});
