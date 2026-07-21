import subprocess, json
sql = "SELECT name, slots FROM profiles WHERE name = 'Opex';"
r = subprocess.run(
    ["docker", "exec", "docker-postgres-1", "psql", "-U", "opex", "-d", "opex", "-t", "-A", "-c", sql],
    capture_output=True, text=True
)
print("raw:", r.stdout[:500])
# Parse JSON
if r.stdout.strip():
    parts = r.stdout.strip().split("|", 1)
    if len(parts) == 2:
        name, slots_json = parts
        slots = json.loads(slots_json)
        for slot_name, entries in slots.items():
            print(f"\n  slot: {slot_name}")
            for e in entries:
                print(f"    provider={e.get('provider','?')} model={e.get('model','?')}")
