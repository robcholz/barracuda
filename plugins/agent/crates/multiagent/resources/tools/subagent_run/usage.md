The child does not see the parent conversation; make `goal` complete and
standalone. This tool blocks the current agent until the child finishes. Do not
use it by default. Prefer `subagent_spawn`, whose result is delivered
automatically, unless the current step explicitly requires the child result
before any other useful work can continue.

At most 3 subagents may be live at once across the whole tree, nested ones
included. A spawn beyond that is rejected; wait for a result or delete a
subagent you no longer need first.
