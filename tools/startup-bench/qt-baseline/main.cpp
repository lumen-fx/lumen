// Qt6 Widgets apples-to-apples baseline for the Lumen `counter` app.
//
// Widget census (matches counter/main.lmn ~11 nodes): 1 top-level
// QWidget window, a scroll-area stand-in QWidget holding 8 colored
// "tile" QLabels in a grid, plus 1 status QLabel and 2 QPushButtons -
// a handful of labels + buttons, same order of magnitude as the Lumen
// tree. Real init/font/paint paths are exercised: QApplication brings
// up the platform plugin (offscreen), fontconfig, and the raster paint
// engine; show() + one event-loop turn forces a real first paint.
//
// Runs under QT_QPA_PLATFORM=offscreen so no window ever hits the
// author's compositor. Quits after the first frame is painted so the
// measured wall time is exec -> first-frame-ready, matching the Lumen
// `--headless --ticks 1` definition.

#include <QApplication>
#include <QWidget>
#include <QLabel>
#include <QPushButton>
#include <QGridLayout>
#include <QVBoxLayout>
#include <QTimer>

int main(int argc, char **argv) {
    QApplication app(argc, argv);

    QWidget window;
    window.setWindowTitle("Qt baseline");
    window.resize(480, 640);

    auto *root = new QVBoxLayout(&window);

    // 8 colored tiles (mirror counter's 8 <tile>s).
    auto *tiles = new QWidget(&window);
    auto *grid = new QGridLayout(tiles);
    const char *colors[8] = {
        "#dc4548", "#338cea", "#48ca6b", "#edb033",
        "#b557d9", "#33c7ce", "#e85e9c", "#949433",
    };
    for (int i = 0; i < 8; ++i) {
        auto *tile = new QLabel(QString("Tile %1").arg(i + 1), tiles);
        tile->setAlignment(Qt::AlignCenter);
        tile->setStyleSheet(
            QString("background:%1; color:white;").arg(colors[i]));
        tile->setMinimumSize(100, 60);
        grid->addWidget(tile, i / 4, i % 4);
    }
    root->addWidget(tiles);

    // Status label (mirrors counter's bound label).
    auto *status = new QLabel("Qt - baseline", &window);
    status->setAlignment(Qt::AlignCenter);
    root->addWidget(status);

    // A couple of interactive controls.
    root->addWidget(new QPushButton("Count", &window));
    root->addWidget(new QPushButton("Reset", &window));

    window.show();

    // Quit as soon as the event loop has processed the initial
    // expose/paint. singleShot(0) fires after the first round of posted
    // events (incl. the paint), so exec() returns just past first frame.
    QTimer::singleShot(0, &app, &QApplication::quit);
    return app.exec();
}
