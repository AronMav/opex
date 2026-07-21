import subprocess
# Check the MediaAttachment wire format - what fields does it have?
sql = "SELECT id, mime, size_bytes, filename, created_at FROM uploads WHERE owner_type = 'client_upload' AND created_at > '2026-07-21 14:09' ORDER BY created_at ASC LIMIT 10;"
r = subprocess.run(
    ["docker", "exec", "docker-postgres-1", "psql", "-U", "opex", "-d", "opex", "-c", sql],
    capture_output=True, text=True
)
print("=== Uploads ===")
print(r.stdout[:2000])

# Check what columns the uploads table has
sql2 = "SELECT column_name FROM information_schema.columns WHERE table_name = 'uploads' ORDER BY ordinal_position;"
r2 = subprocess.run(
    ["docker", "exec", "docker-postgres-1", "psql", "-U", "opex", "-d", "opex", "-c", sql2],
    capture_output=True, text=True
)
print("\n=== Uploads columns ===")
print(r2.stdout[:1000])
