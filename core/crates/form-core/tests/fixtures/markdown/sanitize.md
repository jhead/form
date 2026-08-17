Links the app can act on and links it cannot.

- [https](https://example.com/ok)
- [http](http://example.com/ok)
- [mail](mailto:someone@example.com)
- [file](file:///Users/x/notes.txt)
- [script](javascript:alert(1))
- [mixed case](JaVaScRiPt:alert(1))
- [data](data:text/html,<script>x</script>)
- [relative](./sibling.md)

![blocked](javascript:alert(1))

<div onclick="steal()"><script>evil()</script></div>

An inline <img src=x onerror=alert(1)> tag.
