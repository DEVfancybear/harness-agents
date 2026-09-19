// Read a raw PTY transcript the way a terminal would, and print the screen.
//
// The PTY acceptance cases capture bytes, not a screen: in raw mode this console
// does not translate line feeds, so a transcript can look like one long line even
// though the app positioned every row with an escape sequence. This script applies
// the escape sequences the app actually emits - cursor movement, erase, scrolling -
// and prints the resulting grid, so an inserted line can be checked where it
// really lands. It is evidence tooling, not part of the product.
//
// Usage: node scripts/Read-HaTranscript.mjs <transcript-file> [--tail-bytes N]

import { readFileSync } from 'node:fs';

const DEFAULT_SCROLLBACK = 400;

function createScreen(columns, rows) {
    const scrollback = [];
    let grid = Array.from({ length: rows }, () => Array.from({ length: columns }, () => ' '));
    let x = 0;
    let y = 0;
    let savedX = 0;
    let savedY = 0;

    const ensureRow = (row) => {
        if (row < 0) {
            return;
        }
        while (grid.length <= row) {
            grid.push(Array.from({ length: columns }, () => ' '));
        }
    };

    const scrollUp = () => {
        const [first] = grid.splice(0, 1);
        scrollback.push(first.map((cell) => cell).join('').replace(/\s+$/, ''));
        grid.push(Array.from({ length: columns }, () => ' '));
    };

    // A line feed moves the cursor down; when it is already on the last row the
    // terminal scrolls instead and the cursor stays on that row.
    const newline = () => {
        if (y + 1 >= rows) {
            scrollUp();
        } else {
            y += 1;
        }
        ensureRow(y);
    };

    const put = (character) => {
        if (x >= columns) {
            x = 0;
            newline();
        }
        ensureRow(y);
        grid[y][x] = character;
        x += 1;
    };

    const parseCsi = (params, final) => {
        const numbers = params
            .replace(/^[?>=]/, '')
            .split(';')
            .map((value) => (value === '' ? null : Number.parseInt(value, 10)));
        const first = numbers[0] ?? 1;
        switch (final) {
            case 'H':
            case 'f':
                y = Math.max(0, (numbers[0] ?? 1) - 1);
                x = Math.max(0, (numbers[1] ?? 1) - 1);
                ensureRow(y);
                break;
            case 'A':
                y = Math.max(0, y - first);
                break;
            case 'B':
                newline();
                break;
            case 'C':
                x += first;
                break;
            case 'D':
                x = Math.max(0, x - first);
                break;
            case 'G':
                x = Math.max(0, first - 1);
                break;
            case 'd':
                y = Math.max(0, first - 1);
                ensureRow(y);
                break;
            case 'J':
                if ((numbers[0] ?? 0) === 2) {
                    grid = Array.from({ length: rows }, () => Array.from({ length: columns }, () => ' '));
                } else {
                    for (let column = x; column < columns; column += 1) {
                        ensureRow(y);
                        grid[y][column] = ' ';
                    }
                }
                break;
            case 'K': {
                ensureRow(y);
                const mode = numbers[0] ?? 0;
                const from = mode === 2 ? 0 : mode === 1 ? 0 : x;
                const to = mode === 1 ? x : columns;
                for (let column = from; column < to; column += 1) {
                    grid[y][column] = ' ';
                }
                break;
            }
            case 'S':
                for (let index = 0; index < first; index += 1) {
                    scrollUp();
                }
                break;
            case 's':
                savedX = x;
                savedY = y;
                break;
            case 'u':
                x = savedX;
                y = savedY;
                break;
            default:
                break;
        }
    };

    return {
        write(text) {
            for (let index = 0; index < text.length; index += 1) {
                const character = text[index];
                if (character === '\u001b') {
                    const next = text[index + 1];
                    if (next === '[') {
                        let end = index + 2;
                        while (end < text.length && !/[@-~]/.test(text[end])) {
                            end += 1;
                        }
                        parseCsi(text.slice(index + 2, end), text[end]);
                        index = end;
                        continue;
                    }
                    if (next === ']') {
                        // OSC ... BEL or ST
                        let end = index + 2;
                        while (end < text.length && text[end] !== '\u0007' && text[end] !== '\u001b') {
                            end += 1;
                        }
                        index = text[end] === '\u001b' ? end + 1 : end;
                        continue;
                    }
                    index += 1;
                    continue;
                }
                if (character === '\r') {
                    x = 0;
                    continue;
                }
                if (character === '\n') {
                    newline();
                    continue;
                }
                if (character === '\u0007') {
                    continue;
                }
                if (character < ' ') {
                    continue;
                }
                put(character);
            }
        },
        render() {
            const rowsOut = grid.map((row) => row.join('').replace(/\s+$/, '')).map((row) => row.trimEnd());
            while (rowsOut.length > 0 && rowsOut[rowsOut.length - 1] === '') {
                rowsOut.pop();
            }
            return { scrollback, rows: rowsOut, cursor: { x, y } };
        },
    };
}

const [, , file, ...rest] = process.argv;
if (!file) {
    console.error('usage: node scripts/Read-HaTranscript.mjs <transcript-file> [--tail-bytes N] [--columns N] [--rows N]');
    process.exit(2);
}
const option = (name, fallback) => {
    const index = rest.indexOf(name);
    return index >= 0 && rest[index + 1] ? Number.parseInt(rest[index + 1], 10) : fallback;
};
const columns = option('--columns', 110);
const rows = option('--rows', 30);
const tailBytes = option('--tail-bytes', 0);

let bytes = readFileSync(file);
if (tailBytes > 0 && bytes.length > tailBytes) {
    bytes = bytes.subarray(bytes.length - tailBytes);
}
const screen = createScreen(columns, rows);
screen.write(bytes.toString('utf8'));
const { scrollback, rows: rendered, cursor } = screen.render();

if (scrollback.length > 0) {
    console.log(`--- scrolled off (${scrollback.length} rows, showing last ${Math.min(scrollback.length, DEFAULT_SCROLLBACK)}) ---`);
    for (const row of scrollback.slice(-DEFAULT_SCROLLBACK)) {
        console.log(`| ${row}`);
    }
}
console.log(`--- screen (${columns}x${rows}, cursor ${cursor.x},${cursor.y}) ---`);
for (const row of rendered) {
    console.log(`| ${row}`);
}
