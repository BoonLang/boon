---
active: true
iteration: 1
max_iterations: 0
completion_promise: "COMPLETE"
started_at: "2025-12-20T12:27:30Z"
---

Fix broken Clear completed logic in todo_mvc.bn example, scenario: 1) Check
  a todo. 2) Click Clear completed button. 3) Add a new todo. 4) Notice many "[VALUE_ACTOR] Subscription
  reply receiver dropped for '(ValueActor ["PersistenceId: 019b3b8ea4e0575e2750832a98f5b219", "8"]
  'ElementCheckbox[event] (derived)')'" in browser log, first ones appear just on app start, look
  suspicious. 5) Hover on new todo and press X to remove it (update mcp tools or find another way how to
  do it automatically if you have problem with it). 6) Notice that previouly cleared todo(s) by Clear
  completed button are back (the bug) and also more extra messages apper in browser log. ; Verify it works by testing the exact scenario and that not excessive receiver drops are appearing in broeser log. List/remove and List/append have to be supported multiple times on one List (there may be multiple filters on differnt part of List transformation pipeline).

Output <promise>COMPLETE</promise> when done.
