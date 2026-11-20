create or replace function widget_by_id(id text) returns table (id text, total int) as $$
  select id, total from widgets where widgets.id = id;
$$ language sql;
