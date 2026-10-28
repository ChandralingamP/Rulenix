import subprocess
from pathlib import Path

repo_root = Path(r"C:\GitHub\TradingSVCV\Rulenix")
output_dir = repo_root / "postgres-init"
output_dir.mkdir(exist_ok=True)
output_file = output_dir / "01-init.sql"

# Get all migration files from git history
cmd = ["git", "ls-tree", "-r", "--name-only", "9bf159f^"]
res = subprocess.run(cmd, cwd=repo_root, capture_output=True, text=True, check=True)
migration_files = sorted([line.strip() for line in res.stdout.splitlines() if line.startswith("backend/migrations/") and line.endswith(".sql")])

print(f"Found {len(migration_files)} migration files.")

with open(output_file, "w", encoding="utf-8") as out:
    out.write("-- Rulenix Unified Database Initialization\n\n")
    for mf in migration_files:
        out.write(f"\n-- >>> START: {mf} <<<\n")
        file_cmd = ["git", "show", f"9bf159f^:{mf}"]
        file_res = subprocess.run(file_cmd, cwd=repo_root, capture_output=True, text=True, check=True)
        out.write(file_res.stdout)
        out.write(f"\n-- <<< END: {mf} >>>\n\n")

print(f"Written to {output_file} ({output_file.stat().st_size} bytes).")

