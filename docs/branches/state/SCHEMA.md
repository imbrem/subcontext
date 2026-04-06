# State Branch Schema

The `state` branch tracks global metadata for a subcontext install: registered
objects (tasks, children) and pool references.

## Layout

```
SCHEMA.md
state.db
```

## state.db

SQLite database with two tables:

### `objects`

Registry of all known UUIDs and their owners.

| Column     | Type | Constraint                              |
|------------|------|-----------------------------------------|
| uuid       | TEXT | PRIMARY KEY                             |
| owner_uuid | TEXT | NOT NULL                                |
| owner_type | TEXT | NOT NULL, CHECK IN ('pool', 'child')    |

### `pools`

Tracks each pool branch and its last-known commit.

| Column         | Type | Constraint  |
|----------------|------|-------------|
| uuid           | TEXT | PRIMARY KEY |
| current_commit | TEXT | NOT NULL    |
