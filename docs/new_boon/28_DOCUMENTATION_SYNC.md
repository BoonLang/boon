## Documentation Sync Needed

After plan approval, the following changes must be synced to docs/new_engine/:

### OVERVIEW.md Updates Required:
1. **ScopeId propagation rules** (line 99-104): Remove HOLD iteration, already fixed in plan
2. **Payload definition** (line 364 vs 640): Unify to single definition per K13
3. **Add TextTemplate node** to node types table (around line 377)
4. **Timer implementation** (line 599): Add wall-clock integration per K6
5. **GraphSnapshot** (line 614): Expand per K9 and add NodeSnapshot definition per K24
6. **Success criteria** (line 843): Change "No Arcs" to "No Arc<ValueActor>"
7. **Add sections for:** Alias field-paths (K14), LINK binding protocol (K15), Effect nodes (K18), BLOCK compilation (K19), Tick quiescence (K20)

### EXAMPLE_COMPATIBILITY.md Updates Required:
1. **Fix line 508**: Change `item_index` to `item_key` (critical bug)
2. **Add analysis** for 10 missing examples (list_retain_count, list_object_state, etc.)
3. **Mark outdated** the 4 examples not in playground (latest, then, when, while)
4. **Update header** to say "23 examples" not "all playground examples"

### README.md Updates Required:
1. Add Known Issues reference
2. Update success criteria text
3. Add TextTemplate to node mapping table

---

