import subprocess
sid = "2c66aad4-526b-41d8-a63e-389f3f75c262"
# Get all user messages
sql = f"SELECT role, left(content, 800), created_at FROM messages WHERE session_id = '{sid}' AND role = 'user' ORDER BY created_at ASC LIMIT 5;"
r = subprocess.run(
    ["docker", "exec", "docker-postgres-1", "psql", "-U", "opex", "-d", "opex", "-c", sql],
    capture_output=True, text=True
)
print("=== User messages ===")
print(r.stdout[:3000])

# Get all messages count + tool_calls
sql2 = f"SELECT count(*) as total, count(*) FILTER (WHERE tool_calls::text LIKE '%file_handler%') as fh_calls, count(*) FILTER (WHERE tool_calls::text LIKE '%save%') as save_calls, count(*) FILTER (WHERE tool_calls::text LIKE '%workspace_read%') as wr_calls FROM messages WHERE session_id = '{sid}';"
r2 = subprocess.run(
    ["docker", "exec", "docker-postgres-1", "psql", "-U", "opex", "-d", "opex", "-t", "-A", "-F", "|", "-c", sql2],
    capture_output=True, text=True
)
print(f"\n=== Stats: {r2.stdout.strip()} ===")
