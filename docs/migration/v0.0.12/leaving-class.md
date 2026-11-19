# An element `<if>` or `<for>` removes first gains the class `leaving`

This affects apps whose stylesheet already uses a class named `leaving` for
something else.

When the stylesheet names `leaving` in any selector, an element an `<if>` or
a `<for>` takes out of the tree is no longer removed at once: it gains the
class, stays while an animation the class starts runs, and goes when it ends.
An app that used `leaving` for its own purposes now sees its rows and `<if>`
bodies keep that class, and stay for the length of any animation it starts,
on their way out.

Rename your own class to keep the old behaviour:

```css
/* before */
.item.leaving { opacity: 0.5; }
/* after */
.item.on-the-way-out { opacity: 0.5; }
```
