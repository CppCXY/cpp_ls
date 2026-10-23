// A trailing comment.
/* A block comment. */
/**
 * A doc comment.
 */
int commented(int a) {
    int b = a; // after a statement
    /* before a statement */ b += 1;
    if (b > 0) { // after a brace
        b = 0;
    } else {
        b = 1; /* inline */
    }
    return
    b; // broken return
}
