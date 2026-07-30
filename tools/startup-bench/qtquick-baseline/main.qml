import QtQuick

// Mirrors counter/main.lmn: 8 colored tiles, a status label, two buttons.
Rectangle {
    width: 480
    height: 640
    color: "#0c1c30"

    Column {
        anchors.centerIn: parent
        spacing: 16

        Grid {
            columns: 4
            spacing: 8
            Repeater {
                model: ["#dc4548", "#338cea", "#48ca6b", "#edb033",
                        "#b557d9", "#33c7ce", "#e85e9c", "#949433"]
                Rectangle {
                    width: 100
                    height: 60
                    radius: 8
                    color: modelData
                    Text {
                        anchors.centerIn: parent
                        text: "Tile"
                        color: "white"
                    }
                }
            }
        }

        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: "Qt Quick - baseline"
            color: "white"
        }

        Row {
            anchors.horizontalCenter: parent.horizontalCenter
            spacing: 12
            Rectangle {
                width: 120; height: 44; radius: 22; color: "#163459"
                Text { anchors.centerIn: parent; text: "Count"; color: "white" }
                MouseArea { anchors.fill: parent }
            }
            Rectangle {
                width: 120; height: 44; radius: 22; color: "#163459"
                Text { anchors.centerIn: parent; text: "Reset"; color: "white" }
                MouseArea { anchors.fill: parent }
            }
        }
    }
}
