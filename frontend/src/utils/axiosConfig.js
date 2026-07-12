import axios from "axios";
import { API_BASE_URL } from "./constants.js";

const readCookie = (name) => document.cookie
  .split(";")
  .map((part) => part.trim().split("="))
  .find(([key]) => key === name)?.slice(1).join("=");

const attachSecurity = (config) => {
  config.withCredentials = true;
  if (["post", "put", "patch", "delete"].includes(config.method?.toLowerCase())) {
    const csrf = readCookie("rulenix_csrf");
    if (csrf) {
      const value = decodeURIComponent(csrf);
      if (typeof config.headers?.set === "function") config.headers.set("X-CSRF-Token", value);
      else config.headers = { ...config.headers, "X-CSRF-Token": value };
    }
  }
  return config;
};

if (axios.defaults) axios.defaults.withCredentials = true;
axios.interceptors.request.use(attachSecurity);
axios.interceptors.response.use(undefined, (error) => {
  if (error.response?.status === 401 && typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent("rulenix:unauthorized"));
  }
  return Promise.reject(error);
});

const apiClient = axios.create({
  baseURL: API_BASE_URL,
  headers: { "Content-Type": "application/json", Accept: "application/json" },
  withCredentials: true,
});
apiClient.interceptors.request.use(attachSecurity);
apiClient.interceptors.response.use(undefined, (error) => {
  if (error.response?.status === 401 && typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent("rulenix:unauthorized"));
  }
  return Promise.reject(error);
});

export default apiClient;
