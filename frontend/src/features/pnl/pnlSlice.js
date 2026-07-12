import { createAsyncThunk, createSlice } from "@reduxjs/toolkit";
import apiClient from "../../utils/axiosConfig.js";
import { getAuthUsername } from "../../utils/authCookies.js";

export const fetchTrades = createAsyncThunk(
  "pnl/fetchTrades",
  async (
    { page = 1, pageSize = 20, mode = "all" } = {},
    thunkAPI
  ) => {
    try {
      const username = getAuthUsername();
      if (!username) {
        return thunkAPI.rejectWithValue(
          "Session expired. Please sign in again."
        );
      }
      const params = { page, page_size: pageSize };
      if (mode && mode !== "all") {
        params.mode = mode;
      }
      const response = await apiClient.get(`/pnl`, {
        params,
      });
      return response.data;
    } catch (error) {
      return thunkAPI.rejectWithValue(
        error.response?.data?.detail || "Unable to load profit & loss data"
      );
    }
  }
);

export const exportTrades = createAsyncThunk(
  "pnl/exportTrades",
  async (params = {}, thunkAPI) => {
    try {
      const username = getAuthUsername();
      if (!username) {
        return thunkAPI.rejectWithValue(
          "Session expired. Please sign in again."
        );
      }
      const query = { ...params };
      if (query.mode === "all") {
        delete query.mode;
      }
      const response = await apiClient.get(`/pnl/export`, {
        params: query,
        responseType: "blob",
      });
      return response.data;
    } catch (error) {
      return thunkAPI.rejectWithValue(
        error.response?.data?.detail || "Unable to export trades"
      );
    }
  }
);

const initialState = {
  entries: [],
  status: "idle",
  error: null,
  page: 1,
  pageSize: 20,
  totalPages: 1,
  totalRecords: 0,
  totalProfit: 0,
  totalMargin: 0,
  totalBrokerage: 0,
  totalNetProfit: 0,
  exporting: false,
  exportError: null,
  mode: "all",
};

const pnlSlice = createSlice({
  name: "pnl",
  initialState,
  reducers: {
    setPage(state, action) {
      state.page = action.payload;
    },
    setMode(state, action) {
      state.mode = action.payload;
      state.page = 1;
    },
  },
  extraReducers: (builder) => {
    builder
      .addCase(fetchTrades.pending, (state, action) => {
        state.status = "loading";
        state.error = null;
        if (action.meta.arg?.page) {
          state.page = action.meta.arg.page;
        }
        if (action.meta.arg?.pageSize) {
          state.pageSize = action.meta.arg.pageSize;
        }
        if (action.meta.arg?.mode) {
          state.mode = action.meta.arg.mode;
        }
      })
      .addCase(fetchTrades.fulfilled, (state, action) => {
        state.status = "succeeded";
        state.entries = action.payload.results || action.payload.trades || [];
        state.totalRecords =
          action.payload.total_records ?? state.entries.length;
        state.totalPages = action.payload.total_pages ?? 1;
        state.totalProfit = Number(action.payload.total_profit || 0);
        const parseAmount = (value) => {
          const numeric = Number(value);
          return Number.isFinite(numeric) ? numeric : 0;
        };
        state.totalMargin = parseAmount(action.payload.total_margin ?? 0);
        state.totalBrokerage = parseAmount(action.payload.total_brokerage ?? 0);
        state.totalNetProfit = parseAmount(
          action.payload.total_net_profit ?? 0
        );
        if (action.payload.mode) {
          state.mode = action.payload.mode;
        }
      })
      .addCase(fetchTrades.rejected, (state, action) => {
        state.status = "failed";
        state.error = action.payload;
      })
      .addCase(exportTrades.pending, (state) => {
        state.exporting = true;
        state.exportError = null;
      })
      .addCase(exportTrades.fulfilled, (state) => {
        state.exporting = false;
      })
      .addCase(exportTrades.rejected, (state, action) => {
        state.exporting = false;
        state.exportError = action.payload;
      });
  },
});

export const { setPage, setMode } = pnlSlice.actions;

export default pnlSlice.reducer;
