# An element's text takes room beside its children

This affects apps with an element that has both its own text and element
children, such as `<button text="Save"><image src="save.svg" /></button>`.

The text now lays out first, before the children, as it does in a browser.
The element is sized to hold both, its `gap` separates them, and its
`flex-direction`, `padding`, and alignment place the text like a child.
Before, the element was sized by its children alone and the text drew from
its content-box corner, over the children and past the element's edge.

An app that moved the text into a child of its own to get it laid out keeps
working and needs no change. One that left room for the text by hand, with
padding or a fixed width, can drop that now. Text fields (`<input>`,
`<textarea>`) are unchanged.
