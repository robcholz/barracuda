The child does not see the parent conversation; make `goal` complete and
standalone. This tool blocks the current agent until the child finishes. Do not
use it by default. Prefer `subagent_spawn`, whose result is delivered
automatically, unless the current step explicitly requires the child result
before any other useful work can continue.
