# RMS product font

`NotoSansKR-RMS.ttf` is a subset of Noto Sans KR for the Latin, Hangul Jamo, modern Hangul syllable, punctuation, and won-sign ranges used by the RMS product.

The source font is distributed by Google Fonts under the SIL Open Font License in `OFL.txt`.

Regenerate the subset with `pyftsubset NotoSansKR-VF.ttf --output-file=NotoSansKR-RMS.ttf --unicodes=U+0000-00FF,U+1100-11FF,U+3130-318F,U+AC00-D7A3,U+2000-206F,U+20A9 --layout-features=*`.
