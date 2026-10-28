path = "python-backend/app/runtime/service.py"
with open(path, "r", encoding="utf-8") as f:
    content = f.read()

target = """            self.market_data = MarketDataIngestionService(
                self.session_factory, clients, self.settings
                )
                self.supervisor.start(
                    "market_data_ingestion",
                    self.market_data.run_cycle,
                    interval_seconds=60.0,
                timeout_seconds=60.0,
                critical=False,
                )
                self.supervisor.start(
                    "active_contract_ticks",
                    self.market_data.sync_active_contract_ticks,
                    interval_seconds=3.0,
                timeout_seconds=10.0,
                critical=False,
                )"""

replacement = """            self.market_data = MarketDataIngestionService(
                self.session_factory, clients, self.settings
            )
            self.supervisor.start(
                "market_data_ingestion",
                self.market_data.run_cycle,
                interval_seconds=60.0,
                timeout_seconds=60.0,
                critical=False,
            )
            self.supervisor.start(
                "active_contract_ticks",
                self.market_data.sync_active_contract_ticks,
                interval_seconds=3.0,
                timeout_seconds=10.0,
                critical=False,
            )"""

if target in content:
    content = content.replace(target, replacement)
    with open(path, "w", encoding="utf-8", newline="\r\n") as f:
        f.write(content)
    print("Replaced successfully")
else:
    # Try normalized
    target_norm = target.replace("\r\n", "\n")
    content_norm = content.replace("\r\n", "\n")
    if target_norm in content_norm:
        content_norm = content_norm.replace(target_norm, replacement.replace("\r\n", "\n"))
        with open(path, "w", encoding="utf-8", newline="\r\n") as f:
            f.write(content_norm.replace("\n", "\r\n"))
        print("Replaced with newline normalization")
    else:
        print("Not found")

