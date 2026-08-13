# Data lifecycle notes

Free-form, no frontmatter — how data moves through the system from
creation to eventual purge, written up after custom fields and data
retention both shipped and someone asked "wait, what actually happens to
a custom field value over time?"

## From creation to purge

An event row is created with its fixed columns plus whatever custom field
values the account has defined at that point — custom field definitions
are per-account, not per-event, so an event created before a field was
defined simply has no value for that key rather than a null placeholder
retrofitted onto old rows. Deleting a custom field's definition later
removes the *definition*, not the historical values already stored in
existing events' JSON columns — the spec is explicit that deleting a
field definition doesn't touch underlying event data, only what's exposed
going forward.

## Retention window changes mid-stream

Changing an account's retention window (say, from 365 days to 90) doesn't
retroactively purge anything immediately — the nightly purge job simply
starts using the new window on its next run, so there's up to a
24-hour lag between an admin lowering the window and the corresponding
purge actually happening. This was a deliberate choice over an immediate
synchronous purge on window change, since a large account changing its
window shouldn't trigger a big synchronous delete inside the same request
that saved the setting.

## Audit trail entries about data that no longer exists

Because the audit trail logs a description at write time rather than a
live reference to the row it describes, an audit entry about an event
that was later purged remains fully readable — "user X archived 40 rows
matching filter Y" stays meaningful even after those 40 rows are long
gone, since the description was captured as text, not as a foreign key
that would otherwise dangle.

## Custom field values in exports taken before a purge

A CSV export or scheduled report generated before the nightly purge runs
reflects the data as it existed at export time — export and purge are
independent processes with no coordination between them, so a report
scheduled to run at the same time as a purge could, in principle, see
either the pre-purge or post-purge state depending on which job happens
to run first that night. Not currently treated as a bug, but worth
knowing if the two schedules ever need tighter ordering guarantees.
