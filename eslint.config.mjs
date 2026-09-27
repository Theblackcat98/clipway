// Lint for the GNOME Shell extension. Self-contained (no npm install needed);
// the rules follow the spirit of GNOME Shell's own ESLint configuration.
export default [
    {
        files: ['extension/**/*.js'],
        languageOptions: {
            ecmaVersion: 2024,
            sourceType: 'module',
            globals: {
                global: 'readonly',
                log: 'readonly',
                logError: 'readonly',
                print: 'readonly',
                printerr: 'readonly',
                console: 'readonly',
                TextDecoder: 'readonly',
                TextEncoder: 'readonly',
                setTimeout: 'readonly',
                clearTimeout: 'readonly',
                setInterval: 'readonly',
                clearInterval: 'readonly',
            },
        },
        rules: {
            'no-undef': 'error',
            'no-unused-vars': ['error', {argsIgnorePattern: '^_', varsIgnorePattern: '^_'}],
            'no-redeclare': 'error',
            'no-dupe-keys': 'error',
            'no-dupe-class-members': 'error',
            'no-unreachable': 'error',
            'no-const-assign': 'error',
            'no-self-assign': 'error',
            'no-unsafe-finally': 'error',
            'no-fallthrough': 'error',
            'no-empty': ['error', {allowEmptyCatch: false}],
            'no-var': 'error',
            'prefer-const': 'error',
            'eqeqeq': ['error', 'always'],
            'curly': ['error', 'multi-or-nest', 'consistent'],
            'no-restricted-imports': ['error', {
                paths: [
                    {name: 'gi://Gtk', message: 'Gtk must not be imported in the Shell process.'},
                    {name: 'gi://Gdk', message: 'Gdk must not be imported in the Shell process.'},
                    {name: 'gi://Adw', message: 'Adw must not be imported in the Shell process.'},
                ],
            }],
            'no-restricted-properties': ['error',
                {object: 'imports', property: 'lang', message: 'Lang is deprecated.'},
                {object: 'imports', property: 'mainloop', message: 'Mainloop is deprecated.'},
                {object: 'imports', property: 'byteArray', message: 'ByteArray is deprecated.'},
            ],
        },
    },
];
