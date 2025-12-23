## Architecture Comparison Notes

Insights from comparing with other AI architecture proposals:

| Concept | Adopted | Rationale |
|---------|---------|-----------|
| Reuse PersistenceId(u128) | ❌ | Use structural hash SourceId (see K26) |
| Structured ScopeId segments | ❌ | Hash chain for runtime, debug mode for diagnostics |
| Domain in NodeAddress | ✅ | Essential for WebWorker/Server routing |
| AllocSite + InstanceId | ✅ | Better list item identity for persistence |
| DomainRuntime trait | ⏳ | Phase 8+, after single-threaded works |
| TransportEdge nodes | ✅ | Explicit cross-domain edges |
| Container handles concept | ✅ | Mental model, aligns with delta streams |

---

