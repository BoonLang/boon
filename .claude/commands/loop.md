---
description: "Start Ralph Wiggum loop with proper escaping"
allowed-tools: ["Bash"]
---

Initialize the Ralph loop by writing directly to the state file:

```!
mkdir -p .claude

# Write YAML header
cat > .claude/ralph-loop.local.md <<EOF
---
active: true
iteration: 1
max_iterations: 0
completion_promise: "COMPLETE"
started_at: "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
---

EOF

# Append task using single-quoted heredoc (safe for ALL special characters)
cat >> .claude/ralph-loop.local.md <<'TASK_EOF'
$ARGUMENTS

Output <promise>COMPLETE</promise> when done.
TASK_EOF

echo "🔄 Ralph loop activated!"
echo ""
echo "Iteration: 1"
echo "Max iterations: unlimited"
echo "Completion promise: COMPLETE"
echo ""
cat .claude/ralph-loop.local.md | tail -n +8
```

Work on the task above. When you try to exit, the Ralph loop will feed the SAME PROMPT back to you for the next iteration. You'll see your previous work in files and git history, allowing you to iterate and improve.

CRITICAL RULE: You may ONLY output `<promise>COMPLETE</promise>` when the task is completely and unequivocally DONE. Do not output false promises to escape the loop.
