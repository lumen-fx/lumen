// Qt6 Quick (QML) baseline for the Lumen `counter` app: the fair peer.
//
// Qt Widgets draws native OS-styled controls through QStyle and paints
// almost nothing itself, so it is not comparable to a runtime that
// composites its own scene. Qt Quick is: it builds a scene graph and
// renders it through the RHI (OpenGL on the same offscreen llvmpipe path
// Lumen's headless wgpu/vello uses), with its own glyph atlas and
// batched geometry. That is the like-for-like startup + paint cost.
//
// The QML tree mirrors the counter census (~11 nodes): 8 colored tiles
// in a grid, 1 status label, 2 button-shaped controls. Runs under
// QT_QPA_PLATFORM=offscreen and quits on the first frame swap, matching
// the Lumen `--headless --ticks 1` definition (exec -> first frame).

#include <QGuiApplication>
#include <QQuickView>
#include <QQuickWindow>
#include <QTimer>
#include <QUrl>

int main(int argc, char **argv) {
    QGuiApplication app(argc, argv);

    QQuickView view;
    view.setResizeMode(QQuickView::SizeRootObjectToView);
    view.resize(480, 640);

    // Quit as soon as the scene graph has rendered and swapped its first
    // frame. frameSwapped fires on the GUI thread after the buffer swap.
    bool done = false;
    QObject::connect(&view, &QQuickWindow::frameSwapped, &app, [&]() {
        if (!done) {
            done = true;
            QTimer::singleShot(0, &app, &QGuiApplication::quit);
        }
    });

    view.setSource(QUrl(QStringLiteral("qrc:/main.qml")));
    view.show();

    // Safety net: never hang the harness if no frame ever swaps.
    QTimer::singleShot(5000, &app, &QGuiApplication::quit);
    return app.exec();
}
