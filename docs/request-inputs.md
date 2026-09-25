# Request inputs on release/v0.1

The JSON execution entry points (stdin with no subcommand and `run-compiled`)
reject a top-level `pins` field. This includes `pins: null` and `pins: []`.
This engine line does not support request pins: remove the field to request
ordinary evaluation, or use an engine with pin support and compatible artifacts
when an override is required. Other unknown request fields retain their existing
handling. The same check applies when deserializing the Rust request types,
including through the wasm JSON bindings.

Both explain and fast requests validate input spells after resolving input names
to the program's canonical names. Two records with the same canonical name,
entity ID, and interval start must have equal typed values. Conflicting values
return an `ambiguous input` error naming the input, entity ID, and start date.
Merge or split the spells so that each start date has one value. Different end
dates do not break a tie. Equal duplicate records are accepted. Validation covers
the entire supplied dataset, including records outside the requested periods or
entities and requests with no queries.

Among records that cover the whole query period, the latest interval start wins.
This applies to inputs on queried and related entities in both modes, independent
of record order. A newer record that covers only part of the period does not
displace an older covering record. Fast requests with different query periods
retain the existing fallback to explain, which selects inputs for each period.

These checks apply through `execute_request` and `execute_compiled_request`.
They do not change the lower-level `Engine::new`, dense or lifetime interfaces,
artifact versions, relation precedence, formula selection, or trace format.
The published schemas cover RuleSpec modules, test fixtures, and compiled
artifacts; they do not define execution requests, so their wire formats are
unchanged by this backport.
