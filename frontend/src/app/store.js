import { configureStore } from "@reduxjs/toolkit";
import homeReducer from "../features/home/homeSlice";
import pnlReducer from "../features/pnl/pnlSlice";
import strategiesReducer from "../features/strategies/strategiesSlice.js";

export const store = configureStore({
  reducer: {
    home: homeReducer,
    pnl: pnlReducer,
    strategies: strategiesReducer,
  },
});
