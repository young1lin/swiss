/* The type mirror of this vendored shim (docs/36 D7): the .js beside it is the served
   truth (byte-identical upstream bundle + loader), this file only types the export the
   panel imports - loadXterm hands back the Terminal class, The shim never changes with the migration; upgrading the
   vendored package means upgrading this mirror in the same commit. */
export declare function loadXterm(): Promise<XtermTerminalCtor>;
