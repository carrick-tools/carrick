/**
 * #3117: naming a declaration the entry exports under another name needs the
 * declaration file's own exports. The program for that file is built once per
 * file, however many names are asked of it.
 */
export {};
