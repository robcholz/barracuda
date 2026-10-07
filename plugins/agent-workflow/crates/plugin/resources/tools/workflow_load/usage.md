Call `workflow_actions` first, then construct a complete Workflow definition.
`workflow_load` validates every Action address, argument link, and adjacent
response/request schema before persisting the definition.
A `$previous.output.<field>` argument is valid when one of the previous
Action's response shapes, usually its success shape, has that field with the
same schema; if the Action returns another shape, such as an error, that
execution fails at the step that reads the field.
