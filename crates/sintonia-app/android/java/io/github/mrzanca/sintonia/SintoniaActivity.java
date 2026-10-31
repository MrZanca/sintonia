package io.github.mrzanca.sintonia;

import android.app.AlertDialog;
import android.app.NativeActivity;
import android.content.ActivityNotFoundException;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.provider.OpenableColumns;
import android.view.WindowManager;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;

/**
 * L'activity di Sintonia: una NativeActivity (tutta l'app sta in libsintonia_app.so) più le poche
 * cose che da Rust non si raggiungono. La principale è il risultato del selettore di file
 * (Storage Access Framework), che Android consegna solo a onActivityResult.
 *
 * Rust chiama i metodi pubblici con JNI (crates/sintonia-app/src/android.rs) e registra
 * onFilePicked all'avvio.
 */
public class SintoniaActivity extends NativeActivity {
    private static final int PICK_PLAYLIST = 1;

    /** Il file scelto, copiato nella cartella dei dati; null se annullato o illeggibile. */
    private static native void onFilePicked(String path);

    /** Apre il selettore di file di Android per una playlist M3U. */
    public void pickPlaylist() {
        // classi anonime e non lambda: android.jar non ha il necessario per compilarle (Java 8)
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
                intent.addCategory(Intent.CATEGORY_OPENABLE);
                // le playlist non hanno un tipo MIME affidabile (spesso application/octet-stream)
                intent.setType("*/*");
                try {
                    startActivityForResult(intent, PICK_PLAYLIST);
                } catch (ActivityNotFoundException e) {
                    onFilePicked(null);
                }
            }
        });
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != PICK_PLAYLIST) {
            return;
        }
        final Uri uri = resultCode == RESULT_OK && data != null ? data.getData() : null;
        if (uri == null) {
            onFilePicked(null);
            return;
        }
        // una playlist può pesare decine di MB: la copia non deve fermare l'interfaccia
        new Thread(new Runnable() {
            @Override
            public void run() {
                onFilePicked(copy(uri));
            }
        }, "sintonia-playlist").start();
    }

    /**
     * Copia il documento in files/playlist: il core legge i file con un percorso, e un permesso
     * su un content:// non sopravvive al riavvio. Reimportare lo stesso nome lo aggiorna.
     */
    private String copy(Uri uri) {
        File dir = new File(getFilesDir(), "playlist");
        if (!dir.isDirectory() && !dir.mkdirs()) {
            return null;
        }
        String name = displayName(uri);
        File target = new File(dir, name);
        File partial = new File(dir, name + ".tmp");
        try (InputStream in = getContentResolver().openInputStream(uri);
             OutputStream out = new FileOutputStream(partial)) {
            if (in == null) {
                return null;
            }
            byte[] buffer = new byte[64 * 1024];
            int n;
            while ((n = in.read(buffer)) > 0) {
                out.write(buffer, 0, n);
            }
        } catch (IOException | SecurityException e) {
            partial.delete();
            return null;
        }
        if (!partial.renameTo(target)) {
            partial.delete();
            return null;
        }
        return target.getAbsolutePath();
    }

    /** Il nome del documento, senza caratteri che non vanno in un nome di file. */
    private String displayName(Uri uri) {
        String name = null;
        try (Cursor cursor = getContentResolver().query(
                uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                name = cursor.getString(0);
            }
        } catch (RuntimeException e) {
            // un fornitore di documenti difettoso: si usa il nome di ripiego
        }
        if (name == null || name.trim().isEmpty() || name.startsWith(".")) {
            name = "playlist.m3u";
        }
        return name.replaceAll("[/\\\\:*?\"<>|]", "_");
    }

    /** Apre una pagina nel browser (l'avviso della versione nuova). */
    public void openUrl(final String url) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                try {
                    startActivity(new Intent(Intent.ACTION_VIEW, Uri.parse(url)));
                } catch (ActivityNotFoundException e) {
                    // nessun browser: non c'è niente da fare
                }
            }
        });
    }

    /**
     * Schermo sempre acceso mentre si guarda qualcosa. Sul thread dell'interfaccia di Android:
     * ANativeActivity_setWindowFlags chiamato da Rust prima che la finestra esista bloccava l'app.
     */
    public void keepScreenOn(final boolean on) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                if (on) {
                    getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
                } else {
                    getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
                }
            }
        });
    }

    /** Un messaggio con il solo pulsante OK (al posto delle finestre di messaggio del PC). */
    public void showMessage(final String title, final String text) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                new AlertDialog.Builder(SintoniaActivity.this)
                        .setTitle(title)
                        .setMessage(text)
                        .setPositiveButton(android.R.string.ok, null)
                        .show();
            }
        });
    }
}
