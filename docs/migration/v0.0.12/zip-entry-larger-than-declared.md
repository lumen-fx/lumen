# A zip entry larger than it declares fails the extraction

`archive::extract` on a zip whose entry holds more bytes than its headers
declare now fires `archive_error`, where it used to write the whole
entry. An archive that a standard tool writes is unaffected; one that
starts failing here is malformed and needs rebuilding.
