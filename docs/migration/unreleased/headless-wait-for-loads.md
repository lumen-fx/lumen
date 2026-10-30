# Headless options can wait for loads

This affects Rust code that builds `HeadlessOptions` with a struct literal
naming every field. The struct gains `wait_for_loads: bool`, which keeps a
bounded run ticking until nothing the page shows is still loading. Give it
`wait_for_loads: false` to keep the old behaviour, or end the literal with
`..HeadlessOptions::default()`.
