import { createAsyncThunk, createSlice } from "@reduxjs/toolkit";
import apiClient from "../../utils/axiosConfig.js";
import { getAuthUsername } from "../../utils/authCookies.js";

const requireUsername = (thunkAPI) => {
  const username = getAuthUsername();
  return username || thunkAPI.rejectWithValue("Session expired. Please sign in again.");
};

export const fetchStrategies = createAsyncThunk(
  "strategies/fetchStrategies",
  async ({ silent = false } = {}, thunkAPI) => {
    const username = requireUsername(thunkAPI);
    if (typeof username !== "string") return username;
    try {
      const response = await apiClient.get("/strategies");
      return { strategies: response.data?.strategies || [], silent };
    } catch (error) {
      return thunkAPI.rejectWithValue(
        error.response?.data?.detail || "Unable to load strategies"
      );
    }
  }
);

export const setStrategyActivation = createAsyncThunk(
  "strategies/setStrategyActivation",
  async ({ strategyKey, active }, thunkAPI) => {
    const username = requireUsername(thunkAPI);
    if (typeof username !== "string") return username;
    try {
      const response = await apiClient.put(
        `/strategies/${strategyKey}/activation`,
        { active }
      );
      return response.data?.strategies || [];
    } catch (error) {
      return thunkAPI.rejectWithValue(
        error.response?.data?.detail || "Unable to update the strategy"
      );
    }
  }
);

export const saveStrategyInstrument = createAsyncThunk(
  "strategies/saveStrategyInstrument",
  async (
    {
      instrument,
      enabled,
      lots,
      runDaySession = true,
      runEveningSession = true,
    },
    thunkAPI
  ) => {
    const username = requireUsername(thunkAPI);
    if (typeof username !== "string") return username;
    try {
      await apiClient.put("/strategy/futures-breakout", {
        instrument,
        enabled,
        lots,
        run_day_session: runDaySession,
        run_evening_session: runEveningSession,
      });
      const response = await apiClient.get("/strategies");
      return response.data?.strategies || [];
    } catch (error) {
      return thunkAPI.rejectWithValue(
        error.response?.data?.detail || "Unable to update the instrument"
      );
    }
  }
);

const initialState = {
  items: [],
  status: "idle",
  activationKey: null,
  instrumentKey: null,
  error: null,
  notice: null,
};

const strategiesSlice = createSlice({
  name: "strategies",
  initialState,
  reducers: {
    clearStrategyNotice(state) {
      state.notice = null;
      state.error = null;
    },
  },
  extraReducers: (builder) => {
    builder
      .addCase(fetchStrategies.pending, (state, action) => {
        if (!action.meta.arg?.silent || !state.items.length) {
          state.status = state.items.length ? "refreshing" : "loading";
        }
        state.error = null;
      })
      .addCase(fetchStrategies.fulfilled, (state, action) => {
        state.status = "succeeded";
        state.items = action.payload.strategies;
      })
      .addCase(fetchStrategies.rejected, (state, action) => {
        state.status = state.items.length ? "succeeded" : "failed";
        state.error = action.payload;
      })
      .addCase(setStrategyActivation.pending, (state, action) => {
        state.activationKey = action.meta.arg.strategyKey;
        state.error = null;
        state.notice = null;
      })
      .addCase(setStrategyActivation.fulfilled, (state, action) => {
        const changed = state.items.find(
          (item) => item.key === state.activationKey
        );
        state.items = action.payload;
        state.notice = changed?.active
          ? "Strategy deactivated."
          : "Strategy activated. Select the instruments you want to use.";
        state.activationKey = null;
      })
      .addCase(setStrategyActivation.rejected, (state, action) => {
        state.activationKey = null;
        state.error = action.payload;
      })
      .addCase(saveStrategyInstrument.pending, (state, action) => {
        state.instrumentKey = action.meta.arg.instrument;
        state.error = null;
        state.notice = null;
      })
      .addCase(saveStrategyInstrument.fulfilled, (state, action) => {
        state.items = action.payload;
        state.instrumentKey = null;
        state.notice = "Instrument configuration saved.";
      })
      .addCase(saveStrategyInstrument.rejected, (state, action) => {
        state.instrumentKey = null;
        state.error = action.payload;
      });
  },
});

export const { clearStrategyNotice } = strategiesSlice.actions;
export default strategiesSlice.reducer;
