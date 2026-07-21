import subprocess, json
sid = "dc113253-dc26-4522-acb0-650250dab316"
# Get all messages with tool_calls
sql = f"SELECT role, left(content, 300), tool_calls::text, source FROM messages WHERE session_id = '" + sid + "' ORDER BY created_at ASC;"
r = subprocess.run(
    ["docker", "exec", "docker-postgres-1", "psql", "-U", "opex", "-d", "opex", "-t", "-A", "-F", "||", "-c", sql],
    capture_output=True, text=True
)
for i, line in enumerate(r.stdout.strip().split("\n")):
    if not line:
        continue
    parts = line.split("||")
    role = parts[0] if len(parts) > 0 else ""
    content = parts[1][:200] if len(parts) > 1 else ""
    tools = parts[2][:300] if len(parts) > 2 else ""
    source = parts[3] if len(parts) > 3 else ""
    print(f"[{i}] role={role} source={source}")
    if content:
        print(f"    content: {content}")
    if tools and tools != "\\N":
        print(f"    tools: {tools}")
